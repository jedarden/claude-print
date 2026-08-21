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
//!   * **AS-6** — `--verbose`: emits `[claude-print <ms>ms]` timing traces to
//!     stderr that the same run without `--verbose` does not, and the two
//!     stderrs differ.
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

/// Set up a temporary config directory with a malformed config file.
/// Returns the temp dir; caller must keep it alive for the test duration.
fn setup_malformed_config(config_content: &str) -> TempDir {
    let temp_dir = tempfile::tempdir().expect("failed to create temp config dir");
    let config_dir = temp_dir.path().join("claude-print");
    std::fs::create_dir_all(&config_dir).expect("failed to create config dir");
    std::fs::write(config_dir.join("config.toml"), config_content)
        .expect("failed to create malformed config file");
    temp_dir
}

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

// ── AS-6: --verbose timing traces (bf-1bg4) ─────────────────────────────────
//
// Implements AC1 of the parent bead end-to-end: `--verbose` must be externally
// observable. The built binary, run against mock-claude, must emit at least one
// `[claude-print <ms>ms]` timing trace to stderr; the *same* invocation WITHOUT
// `--verbose` must emit none (the tracer is a no-op on the hot path); and the
// two stderrs must differ. That last assertion is the regression guard — if a
// future change moves traces onto the un-flagged path, or drops them from the
// flagged one, this catches it.

/// True if `stderr` contains at least one `[claude-print <ms>ms] ...` timing
/// trace line — the exact shape `Tracer::trace` emits (src/verbose.rs). Hand
/// rolled rather than pulling in the `regex` dev-dependency: the pattern is a
/// fixed prefix `[claude-print ` + one-or-more ASCII digits + `ms]`.
fn has_claude_print_trace(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let Some(rest) = line.strip_prefix("[claude-print ") else {
            return false;
        };
        let n = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        n > 0 && rest[n..].starts_with("ms]")
    })
}

/// `--verbose` surfaces `[claude-print <ms>ms]` traces; without it, none appear.
/// AC1 of parent bf-1bg4: the flag must be observable, and the two stderrs must
/// differ so a regression that silently shifts traces onto (or off of) the
/// flagged path is caught.
#[test]
fn as6_verbose_emits_trace_lines_and_nonverbose_emits_none() {
    // Verbose run: --verbose must surface at least one timing trace on stderr.
    let verbose = run(claude_print().arg("--verbose").arg("test prompt"), BUDGET);
    assert_eq!(
        verbose.code,
        Some(0),
        "AS-6 (verbose): expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        verbose.code,
        verbose.stdout,
        verbose.stderr,
    );
    assert!(
        has_claude_print_trace(&verbose.stderr),
        "AS-6 (verbose): stderr must contain a `[claude-print <ms>ms]` trace \
         line, got:\n{}",
        verbose.stderr,
    );

    // Same invocation WITHOUT --verbose: no trace lines on stderr.
    let quiet = run(claude_print().arg("test prompt"), BUDGET);
    assert_eq!(
        quiet.code,
        Some(0),
        "AS-6 (quiet): expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        quiet.code,
        quiet.stdout,
        quiet.stderr,
    );
    assert!(
        !has_claude_print_trace(&quiet.stderr),
        "AS-6 (quiet): stderr must NOT contain any `[claude-print <ms>ms]` \
         trace line, got:\n{}",
        quiet.stderr,
    );

    // The two stderrs must differ — verbose adds traces the quiet run lacks.
    assert_ne!(
        verbose.stderr, quiet.stderr,
        "AS-6: verbose and non-verbose stderr must differ (verbose adds traces)"
    );
}

// ── EC-7: Stop fires before PROMPT_INJECTED (bf-3i07) ───────────────────────
//
// Implements the plan's "Mock PTY Integration Tests" row: with
// MOCK_STOP_BEFORE_INJECT=1 the mock-claude fixture fires the Stop hook
// immediately, with no trust-dialog output and no delay — before
// claude-print's startup scanner can reach PROMPT_INJECTED. This is the EC-7
// condition: a response to a prompt that was never sent (a session identity
// leak that EC-11 in pty.rs prevents in normal operation; the session.rs
// check is the defense-in-depth backstop). The session must NOT silently
// accept the response — it returns a Setup error (exit 2, is_error:true).

/// `MOCK_STOP_BEFORE_INJECT=1` in text mode → exit 2 and a stderr message that
/// names the EC-7 condition. Text-mode errors are stderr-only, so stdout is
/// empty (no response text leaks through for an unsent prompt).
#[test]
fn ec7_stop_before_inject_text_mode_exit2_stderr() {
    let out = run_with(
        claude_print().arg("test prompt"),
        BUDGET,
        Stdio::null(),
        Some(("MOCK_STOP_BEFORE_INJECT", "1")),
    );

    assert_eq!(
        out.code,
        Some(2),
        "EC-7 (text): expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.is_empty(),
        "EC-7 (text): stdout must be empty (text errors are stderr-only), got:\n{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("EC-7"),
        "EC-7 (text): stderr must name the EC-7 condition, got:\n{}",
        out.stderr
    );
}

/// `MOCK_STOP_BEFORE_INJECT=1` under `--output-format json` → exit 2 and a
/// `result` object on stdout with `is_error: true` — the plan's exact EC-7
/// contract ("exit 2, is_error: true in output").
#[test]
fn ec7_stop_before_inject_json_mode_is_error_true() {
    let out = run_with(
        claude_print()
            .arg("--output-format")
            .arg("json")
            .arg("test prompt"),
        BUDGET,
        Stdio::null(),
        Some(("MOCK_STOP_BEFORE_INJECT", "1")),
    );

    assert_eq!(
        out.code,
        Some(2),
        "EC-7 (json): expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "EC-7 (json): stdout must be valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });
    assert_eq!(v["type"], "result", "EC-7 (json): type must be 'result'");
    assert_eq!(v["is_error"], true, "EC-7 (json): is_error must be true");
    assert_eq!(
        v["subtype"], "internal_error",
        "EC-7 (json): subtype must be the Setup 'internal_error'"
    );
    assert!(
        v.get("error_message")
            .and_then(|m| m.as_str())
            .map(|m| m.contains("EC-7"))
            .unwrap_or(false),
        "EC-7 (json): error_message must name the EC-7 condition, got:\n{}",
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

// ── --dangerously-skip-permissions flag (bf-2v7m) ─────────────────────────────────
//
// Implements the acceptance criteria for bead bf-2v7m: when the flag is NOT
// passed, '--dangerously-skip-permissions' must NOT appear in the child argv;
// when the flag IS passed, it must appear EXACTLY ONCE. This guards against the
// regression where session.rs unconditionally pushed the flag (making the CLI
// flag inert) and main.rs duplicated it when passed.

/// WITHOUT `--dangerously-skip-permissions`: the flag must NOT appear in the
/// child argv at all (permissions are NOT skipped by default).
#[test]
fn dangerously_skip_permissions_flag_absent_when_not_passed() {
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
        "without flag: expected exit 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    let args = read_recorded_argv(&record);

    // Sanity: the relay --settings=<temp>/settings.json is ALWAYS forwarded.
    assert!(
        args.iter().any(|a| a.starts_with("--settings=")),
        "without flag: relay --settings= must be in child argv, got: {:?}",
        args
    );
    // bf-2v7m: --dangerously-skip-permissions must be ABSENT when not passed.
    assert!(
        !args
            .iter()
            .any(|a| a.contains("dangerously-skip-permissions")),
        "without flag: --dangerously-skip-permissions must NOT be in child argv, \
         got: {:?}",
        args
    );
}

/// WITH `--dangerously-skip-permissions`: the flag MUST appear EXACTLY ONCE in
/// the child argv (not duplicated, not missing).
#[test]
fn dangerously_skip_permissions_flag_appears_exactly_once_when_passed() {
    let dir = TempDir::new().unwrap();
    let record = dir.path().join("child_argv");
    let record_str = record.to_string_lossy().into_owned();

    let out = run_with(
        claude_print()
            .arg("--dangerously-skip-permissions")
            .arg("test prompt"),
        BUDGET,
        Stdio::null(),
        Some(("MOCK_RECORD_ARGS", record_str.as_str())),
    );

    assert_eq!(
        out.code,
        Some(0),
        "with flag: expected exit 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    let args = read_recorded_argv(&record);

    // Count occurrences of --dangerously-skip-permissions.
    let count = args
        .iter()
        .filter(|a| a.contains("dangerously-skip-permissions"))
        .count();

    assert_eq!(
        count, 1,
        "with flag: --dangerously-skip-permissions must appear EXACTLY ONCE in \
         child argv, got {} occurrences in {:?}",
        count, args
    );
}

// ── Config parse error handling ────────────────────────────────────────────────

/// Malformed config file with unclosed bracket → exit 2, structured JSON error.
///
/// AC1-AC5 of bead claudepr-ea80e6b2:
/// 1. Creates malformed config file (unclosed bracket, invalid TOML)
/// 2. Runs claude-print with --output-format json
/// 3. Captures exit code (must be 2, not 0)
/// 4. Verifies stdout contains structured error JSON with required fields
/// 5. Verifies NO silent fallback occurs (exit code is 2, not 0)
///
/// This test is expected to FAIL initially, proving the current behavior is wrong
/// (likely silent fallback to defaults with exit 0).
#[test]
fn config_parse_error_unclosed_bracket_exit2_structured_json() {
    // Create a malformed config file with an unclosed bracket
    let malformed_config = r#"[defaults
model = "claude-opus-4-8""#;
    let temp_dir = setup_malformed_config(malformed_config);

    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg(workspace_bin("mock-claude"))
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");
    // Set XDG_CONFIG_HOME to point to the temp config directory
    cmd.env("XDG_CONFIG_HOME", temp_dir.path());

    let out = run(&mut cmd, BUDGET);

    // AC3: Exit code must be 2 (Setup error), not 0 (silent fallback)
    assert_eq!(
        out.code,
        Some(2),
        "config parse error: expected exit 2, got {:?}\n\
         stdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    // AC4: Verify stdout contains structured error JSON
    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "config parse error: stdout must be valid JSON, got parse error: {e}\n  \
             raw stdout:\n{}",
            out.stdout
        )
    });

    // AC4a: Must contain "type":"result"
    assert_eq!(
        v["type"], "result",
        "config parse error: JSON type must be 'result', got: {:?}\nstdout:\n{}",
        v["type"], out.stdout
    );

    // AC4b: Must contain "is_error":true
    assert_eq!(
        v["is_error"], true,
        "config parse error: is_error must be true, got: {:?}\nstdout:\n{}",
        v["is_error"], out.stdout
    );

    // AC4c: Must contain "subtype":"internal_error" (Setup errors map to internal_error)
    assert_eq!(
        v["subtype"], "internal_error",
        "config parse error: subtype must be 'internal_error', got: {:?}\nstdout:\n{}",
        v["subtype"], out.stdout
    );

    // AC4d: Must contain "error_message" with "invalid config" or similar
    assert!(
        v.get("error_message")
            .and_then(|m| m.as_str())
            .map(|m| m.contains("invalid config") || m.contains("config"))
            .unwrap_or(false),
        "config parse error: error_message must mention 'config', got: {:?}\nstdout:\n{}",
        v.get("error_message"),
        out.stdout
    );

    // AC4e: Must contain claude_version field
    assert!(
        v.get("claude_version").and_then(|v| v.as_str()).is_some(),
        "config parse error: must include claude_version field, got: {:?}\nstdout:\n{}",
        v.get("claude_version"),
        out.stdout
    );
}

/// Malformed config with invalid TOML syntax (duplicate keys) → exit 2, structured error.
#[test]
fn config_parse_error_duplicate_keys_exit2_structured_json() {
    let malformed_config = r#"[defaults]
model = "claude-opus-4-8"
model = "claude-haiku-4-5""#;
    let temp_dir = setup_malformed_config(malformed_config);

    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg(workspace_bin("mock-claude"))
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");
    cmd.env("XDG_CONFIG_HOME", temp_dir.path());

    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "duplicate keys config error: expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "duplicate keys error: stdout must be valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });

    assert_eq!(v["type"], "result");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
}

/// Config validation error (model name doesn't start with "claude-") → exit 2.
#[test]
fn config_validation_error_invalid_model_name_exit2_structured_json() {
    let invalid_config = r#"[defaults]
model = "gpt-4""#;
    let temp_dir = setup_malformed_config(invalid_config);

    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg(workspace_bin("mock-claude"))
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");
    cmd.env("XDG_CONFIG_HOME", temp_dir.path());

    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "model validation error: expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "model validation error: stdout must be valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });

    assert_eq!(v["type"], "result");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    // Error message should mention model validation problem
    assert!(
        v.get("error_message")
            .and_then(|m| m.as_str())
            .map(|m| m.contains("model"))
            .unwrap_or(false),
        "model validation error: error_message must mention 'model', got: {:?}",
        v.get("error_message")
    );
}

/// Config validation error (max_turns out of range) → exit 2.
#[test]
fn config_validation_error_max_turns_out_of_range_exit2_structured_json() {
    let invalid_config = r#"[defaults]
max_turns = 0"#;
    let temp_dir = setup_malformed_config(invalid_config);

    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg(workspace_bin("mock-claude"))
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");
    cmd.env("XDG_CONFIG_HOME", temp_dir.path());

    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "max_turns validation error: expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "max_turns validation error: stdout must be valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });

    assert_eq!(v["type"], "result");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    // Error message should mention max_turns validation problem
    assert!(
        v.get("error_message")
            .and_then(|m| m.as_str())
            .map(|m| m.contains("max_turns"))
            .unwrap_or(false),
        "max_turns validation error: error_message must mention 'max_turns', got: {:?}",
        v.get("error_message")
    );
}
