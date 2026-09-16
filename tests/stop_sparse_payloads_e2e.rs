//! Sparse Stop payload regression (bead claudepr-f3ed858a).
//!
//! `docs/notes/hook-design.md` declares every Stop payload field optional for
//! forward compatibility, but the *behavior* under sparse payloads was
//! unspecified and untested: existing beads cover slug validation
//! (claudepr-26e7a0b6) and transcript_path availability (bf-3isy), not payloads
//! that drop fields outright. This file defines and pins the contract at the
//! binary level, across all three output modes:
//!
//! 1. **Derive when possible.** `transcript_path` absent but `session_id` +
//!    `cwd` present → the transcript is read from the DERIVED path
//!    (`$HOME/.claude/projects/<slug>/<session_id>.jsonl`). Proven by
//!    `MOCK_WRITE_DERIVED_JSONL`: the mock writes the JSONL only at the derived
//!    location, so a mis-derivation (wrong slug, wrong directory) cannot pass —
//!    transcript-sourced text carries `num_turns: 1` and non-zero usage where a
//!    `last_assistant_message` fallback carries `num_turns: 0` and zeros.
//! 2. **Derivation impossible → degrade, never crash.** When `session_id` or
//!    `cwd` is absent too, the payload's own `last_assistant_message` becomes
//!    the response (same degraded-success shape as the file-level fallback in
//!    `read_transcript`: `used_fallback`, zero turns/usage, `session_id` from
//!    the payload — `null` when the payload had none).
//! 3. **Nothing to fall back to → bounded setup error.** Exit 2, `error:` on
//!    stderr (text) or a single `internal_error` result object (json /
//!    stream-json after inject). The process must terminate cleanly and within
//!    the wall-clock budget — never hang, never panic.
//! 4. **Unknown extra fields are ignored** end to end.
//!
//! The empty-string variants (`"session_id": ""` etc.) are pinned at the unit
//! level in `src/poller.rs` (`resolve_treats_empty_*`); the mock cannot easily
//! emit them and the resolution contract is identical.
//!
//! Every test drives the COMPILED `claude-print` binary as a subprocess with
//! env injected into the child only (same shape as
//! `tests/stop_duplicate_firings_e2e.rs`), so the process-global `MOCK_*` vars
//! never leak and the tests are parallel-safe. A missing built binary skips
//! rather than fails, mirroring the other binary e2e files.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Locate a workspace bin built alongside this test binary by the workspace
/// build. Test binaries live at `target/<profile>/deps/`; named workspace bins
/// at `target/<profile>/`. Mirrors `tests/binary_e2e.rs::workspace_bin`.
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
/// sparse payload that wedges claude-print fails loudly here, which is the
/// "must not hang" half of the contract.
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

/// Per-session wall-clock budget. mock-claude responds within ~2s; 30s is a
/// generous ceiling that still fails fast on a wedge. Mirrors
/// `tests/binary_e2e.rs::BUDGET`.
const BUDGET: Duration = Duration::from_secs(30);

const RESPONSE: &str = "sparse-payload-response-text";
const SESSION_ID: &str = "mock-session-abc123"; // mock_claude's fixed session id

/// Command builder with the hermetic env every test here needs: temp HOME (the
/// mock writes transcripts under it, never the real `~/.claude`), temp TMPDIR,
/// and the distinctive response text.
fn claude_print_run(
    bin: &std::path::Path,
    mock: &std::path::Path,
    home: &TempDir,
    run_tmp: &TempDir,
) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("--claude-binary").arg(mock).arg("test prompt");
    cmd.env("HOME", home.path())
        .env("TMPDIR", run_tmp.path())
        .env("MOCK_RESPONSE", RESPONSE);
    cmd
}

/// Every non-empty stdout line parses as a JSON object.
fn all_lines_are_json_objects(stdout: &str, case: &str) {
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{case}: stdout line must be valid JSON: {e}\n{line}"));
        assert!(
            v.is_object(),
            "{case}: stdout line must be a JSON object, got: {line}"
        );
    }
}

// ── Contract 1: derive when possible ────────────────────────────────────────

/// `transcript_path` absent, `session_id` + `cwd` present, and the transcript
/// file written ONLY at the derived location: json mode must succeed with
/// transcript-sourced data — `num_turns: 1`, non-zero usage, the payload's
/// session id. A mis-derivation cannot pass: the file would never be found and
/// the result would degrade to `num_turns: 0` / zero usage (the fallback
/// shape).
#[test]
fn derived_path_is_read_when_transcript_path_absent_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_WRITE_DERIVED_JSONL", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "derivation must resolve the transcript: exit 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let result: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "json mode: single result object expected: {e}\n{}",
            out.stdout
        )
    });
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], RESPONSE);
    assert_eq!(
        result["session_id"], SESSION_ID,
        "session_id must come from the sparse payload"
    );
    assert_eq!(
        result["num_turns"], 1,
        "num_turns 1 proves the DERIVED transcript file was read; 0 would mean \
         the last_assistant_message fallback fired instead (mis-derivation)"
    );
    assert!(
        result["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0
            && result["usage"]["output_tokens"].as_u64().unwrap_or(0) > 0,
        "non-zero usage proves transcript-sourced data, got: {}",
        result["usage"]
    );
}

/// Same derivation contract in text mode: the response text reaches stdout
/// exactly once, cleanly.
#[test]
fn derived_path_is_read_when_transcript_path_absent_text_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_WRITE_DERIVED_JSONL", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "text mode: derivation must resolve cleanly\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stderr.contains("error:"),
        "text mode: a derived-path success is not an error, stderr:\n{}",
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        RESPONSE,
        "text mode: stdout must be exactly the response text"
    );
}

/// Same derivation contract in stream-json mode: the reader (which discovers
/// the transcript in the projects dir independently of the Stop payload) must
/// stream the file's events, and every emitted line must be valid JSON. This
/// also pins that mock_claude's slug and claude-print's `cwd_to_slug` agree —
/// a slug divergence leaves the reader watching the wrong directory.
#[test]
fn derived_path_is_streamed_when_transcript_path_absent_stream_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("stream-json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_WRITE_DERIVED_JSONL", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "stream-json: derivation must resolve cleanly\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.is_empty(),
        "stream-json: the discovered transcript events must be streamed to stdout"
    );
    all_lines_are_json_objects(&out.stdout, "stream-json derivation");
    assert!(
        out.stdout.contains(RESPONSE),
        "stream-json: the assistant event carrying the response must be among \
         the streamed lines:\n{}",
        out.stdout
    );
}

// ── Contract 2: derivation impossible → last_assistant_message fallback ─────

/// `transcript_path`, `session_id`, AND `cwd` all absent — derivation is
/// impossible — but the payload still carries `last_assistant_message`: the
/// run degrades to a payload-text SUCCESS in text mode (exit 0, the payload's
/// own response on stdout). A turn that produced an answer must not be
/// discarded because its metadata was sparse.
#[test]
fn derivation_impossible_degrades_to_last_assistant_message_text_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "the payload's own response text must be emitted as a degraded \
         success, not discarded\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        RESPONSE,
        "stdout must be the last_assistant_message text"
    );
    assert!(
        !out.stderr.contains("error:"),
        "text mode: a degraded success is not an error, stderr:\n{}",
        out.stderr
    );
}

/// The same degraded success in json mode: a well-formed `success` result with
/// the fallback text, `session_id: null` (the payload had none), `num_turns: 0`
/// and zero usage (no transcript was read) — the honest degraded shape, never
/// a crash or a malformed object.
#[test]
fn derivation_impossible_degrades_to_last_assistant_message_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "json mode: degraded success exits 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let result: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "json mode: single result object expected: {e}\n{}",
            out.stdout
        )
    });
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], RESPONSE);
    assert_eq!(
        result["session_id"],
        serde_json::Value::Null,
        "session_id must be null — the payload carried none"
    );
    assert_eq!(result["num_turns"], 0, "no transcript was read: zero turns");
    assert_eq!(
        result["usage"]["input_tokens"], 0,
        "no transcript was read: zero usage"
    );
}

/// Degraded success under stream-json: the payload had no transcript to
/// discover and no derivation to make, so the reader streams nothing and the
/// process exits 0 cleanly — no hang, no panic, no malformed output.
#[test]
fn derivation_impossible_degrades_to_last_assistant_message_stream_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("stream-json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "stream-json: degraded success exits 0 within the budget (no hang)\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    // No transcript file exists in the watched projects dir, so the reader has
    // nothing to forward. Whatever this or a future path emits must still be
    // well-formed stream-json.
    all_lines_are_json_objects(&out.stdout, "stream-json degraded success");
}

// ── Contract 3: nothing to fall back to → bounded setup error ───────────────

/// The fully sparse payload — no `transcript_path`, no `session_id`, no `cwd`,
/// no `last_assistant_message` — must produce the BOUNDED setup error in text
/// mode: exit 2 (not a crash/signal), the message on stderr, nothing on stdout,
/// and termination within the budget (no hang).
#[test]
fn fully_sparse_payload_is_bounded_setup_error_text_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1")
        .env("MOCK_OMIT_LAST_MESSAGE", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "text mode: bounded setup error exits 2 (a panic would abort non-zero \
         or signal; a hang would blow the budget)\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.is_empty(),
        "text mode: setup failure must not write stdout: {:?}",
        out.stdout
    );
    assert!(
        out.stderr.contains("error:"),
        "text mode: the error must be reported on stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("no transcript path"),
        "text mode: stderr must explain WHY (no transcript path derivable):\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("panicked"),
        "text mode: a bounded error must be a clean message, not a panic:\n{}",
        out.stderr
    );
}

/// The same bounded setup error in json mode: ONE structured result object on
/// stdout — `subtype: internal_error`, `is_error: true`, an explanatory
/// message — so a JSON caller gets a parseable failure, not empty output.
#[test]
fn fully_sparse_payload_is_bounded_setup_error_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1")
        .env("MOCK_OMIT_LAST_MESSAGE", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "json mode: bounded setup error exits 2\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let result: serde_json::Value = serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "json mode: exactly one error result object expected: {e}\n{}",
            out.stdout
        )
    });
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "internal_error");
    assert_eq!(result["is_error"], true);
    let message = result["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no transcript path"),
        "error_message must explain the sparse payload, got: {message}"
    );
}

/// And in stream-json mode (prompt was injected, so the synthesized error
/// result goes to stdout): every line valid JSON, the failure carried as
/// `internal_error`, exit 2, within budget.
#[test]
fn fully_sparse_payload_is_bounded_setup_error_stream_json_mode() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("stream-json");
    cmd.env("MOCK_OMIT_TRANSCRIPT_PATH", "1")
        .env("MOCK_OMIT_SESSION_ID", "1")
        .env("MOCK_OMIT_CWD", "1")
        .env("MOCK_OMIT_LAST_MESSAGE", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "stream-json: bounded setup error exits 2 within the budget (no hang)\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    all_lines_are_json_objects(&out.stdout, "stream-json bounded error");
    let error_line = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .find(|l| l.contains("\"internal_error\""))
        .unwrap_or_else(|| {
            panic!(
                "stream-json: an internal_error result must be emitted on stdout:\n{}",
                out.stdout
            )
        });
    let result: serde_json::Value = serde_json::from_str(error_line).expect("error line JSON");
    assert_eq!(result["is_error"], true);
}

// ── Contract 4: unknown extra fields ignored ────────────────────────────────

/// A payload carrying fields claude-print has never heard of (a scalar and a
/// nested object) must be processed normally end to end — the parse-level pin
/// lives in `src/poller.rs::parse_payload_unknown_fields_ignored`; this pins
/// it through the whole binary.
#[test]
fn unknown_extra_fields_are_ignored_end_to_end() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = claude_print_run(&bin, &mock, &home, &run_tmp);
    cmd.env("MOCK_UNKNOWN_FIELDS", "1");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "unknown fields must not affect a complete payload\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        RESPONSE,
        "stdout must be exactly the response text"
    );
}
