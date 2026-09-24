//! Binary-level `--help` / `--version` contract tests (claudepr-be05847d).
//!
//! The existing coverage of these two early-exit flags is split and indirect:
//! `tests/version_compat.rs` probes the *installed* `claude` binary (and skips
//! when it is absent), `tests/cli.rs` exercises the parser and the
//! `version_string` helper in-process, and `tests/binary_e2e.rs` only asserts
//! `--version` mentions "claude-print" and "wrapping". Nothing pins `--help`
//! at the binary level at all, and nothing pins that either flag stays
//! hermetic.
//!
//! These tests invoke the *compiled* `claude-print` binary as a subprocess.
//! The claude backend is pinned either to `mock-claude` (a bin of this
//! package, linked by the same build — same resolution strategy as
//! `tests/binary_e2e.rs`) for output pinning, or to an exec-sentinel script
//! that logs every invocation for child accounting. No real Anthropic
//! credentials are required.
//!
//! Pinned contracts:
//!
//!   * **--help** — exit 0, the full help text on *stdout* with an empty
//!     stderr, carrying the about line, the exact usage line, the `serve`
//!     subcommand, and the drop-in-compat flags a `claude -p` caller relies
//!     on.
//!   * **--version** (and `-V`) — exit 0, a single exact line on stdout, an
//!     empty stderr, degrading to `not found` when the claude binary is
//!     missing (the probe's failure is non-fatal).
//!   * **No session machinery** — neither flag starts a session, loads a
//!     prompt, or spawns a session child. Child accounting uses the sentinel:
//!     `--help` must not invoke the claude binary AT ALL (clap exits during
//!     parse, before main()'s body runs anything), while `--version` must
//!     invoke it EXACTLY ONCE, as the documented `<bin> --version` probe
//!     whose output fills the "wrapping claude" clause — never with
//!     session-shaped argv. Prompt inputs supplied alongside either flag are
//!     never loaded: a null stdin with no positional (which exits 4 on the
//!     session path — `binary_e2e::no_prompt_exit4`), a missing
//!     `--input-file` (exit 2/4 on the session path), and a supplied
//!     positional prompt all still exit 0 with the flag's own output.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Per-run wall-clock budget. Both flags exit in milliseconds — the version
/// probe is one subprocess spawn against a fixture that answers instantly —
/// so 15s is a generous ceiling that still fails fast on a wedge instead of
/// hanging the whole `cargo test` run.
const BUDGET: Duration = Duration::from_secs(15);

/// The version string mock-claude answers `--version` probes with
/// (test-fixtures/mock-claude/src/main.rs). Pinning the compiled binary's
/// output against it makes the "wrapping claude" clause fully hermetic.
const MOCK_CLAUDE_VERSION: &str = "mock-claude-version-1.0.0";

/// The exact full line `--version` must print when the backend is mock-claude.
fn expected_version_line() -> String {
    // env! in the test binary and in src/cli.rs resolve against the same
    // package, so this stays correct across version bumps without edits.
    format!(
        "claude-print {} (wrapping claude {})\n",
        env!("CARGO_PKG_VERSION"),
        MOCK_CLAUDE_VERSION
    )
}

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
/// Test binaries live at `target/<profile>/deps/`; named workspace bins live
/// at `target/<profile>/`. Same resolution strategy as
/// `tests/binary_e2e.rs`, `tests/watchdog.rs`, and `tests/pty_integration.rs`.
/// `mock-claude` is a bin target of this package, so any `cargo test` run —
/// plain or filtered — links it into `target/<profile>/` next to the test
/// binaries (claudepr-2c965921).
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
        "mock-claude binary missing at {}; run `cargo build`",
        mock.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(&mock);
    cmd
}

/// Point `claude-print`'s claude binary at the exec-sentinel (see
/// [`exec_sentinel`]) instead of mock-claude.
fn claude_print_sentinel(sentinel: &std::path::Path) -> Command {
    let bin = workspace_bin("claude-print");
    assert!(
        bin.exists(),
        "claude-print binary missing at {}; run `cargo build`",
        bin.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(sentinel);
    cmd
}

/// Materialize the exec-sentinel claude binary in `dir`: a shell script that
/// appends `invoked:<argv>` to `<dir>/invocations.log` on EVERY exec, answers
/// the `--version` probe with a fixed string, and dies with exit 97 on any
/// session-shaped invocation. Unlike mock-claude's MOCK_RECORD_ARGS seam
/// (which deliberately skips `--version` probes so they cannot overwrite the
/// session child's recording), the sentinel accounts for every invocation:
///
///   * invocations.log ABSENT           → the claude binary was never exec'd
///     at all (no probe, no session child, not even a liveness check).
///   * exactly one `invoked:--version`  → the only child was the documented
///     version probe; any session child would leave a session-shaped argv
///     line and exit 97 on the PTY.
///
/// Returns the sentinel's path. `dir` must stay alive (TempDir) for the
/// duration of the test.
fn exec_sentinel(dir: &std::path::Path) -> std::path::PathBuf {
    let log = dir.join("invocations.log");
    let script = dir.join("sentinel-claude");
    let body = format!(
        "#!/bin/sh\n\
         printf 'invoked:%s\\n' \"$*\" >> '{log}'\n\
         if [ \"$1\" = '--version' ]; then\n\
         \x20 printf 'sentinel-claude 9.9.9\\n'\n\
         \x20 exit 0\n\
         fi\n\
         exit 97\n",
        log = log.to_string_lossy(),
    );
    std::fs::write(&script, body).expect("failed to write sentinel script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("failed to chmod sentinel script");
    script
}

/// Read the sentinel's invocation log, or `None` when it was never written.
fn sentinel_invocations(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("invocations.log")).ok()
}

/// Hermetic environment for one run: HOME and XDG_CONFIG_HOME point at a
/// fresh temp dir (the CLI's strict existing-and-writable HOME prerequisite
/// applies to `--version` too), and TMPDIR is isolated so the run cannot
/// observe or touch other tests' temp state.
fn hermetic_env(cmd: &mut Command) -> TempDir {
    let env_dir = tempfile::tempdir().expect("failed to create env temp dir");
    cmd.env("HOME", env_dir.path());
    cmd.env("XDG_CONFIG_HOME", env_dir.path());
    cmd.env("TMPDIR", env_dir.path());
    env_dir
}

/// Run `cmd` to completion, decoding stdout/stderr as UTF-8. If the child has
/// not exited before `budget` elapses it is killed and the test fails — keeps
/// a wedge from hanging the whole `cargo test` run.
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
        .expect("wait_with_output after try wait");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// ── --help: stream routing and exit code ─────────────────────────────────────

/// `claude-print --help` → exit 0 with the help text on STDOUT and an EMPTY
/// stderr. The routing is the contract: an explicit `--help` is a successful
/// query (stdout), unlike a usage error, which clap writes to stderr with
/// exit 2 — callers deciding "is this a success?" read the stream as much as
/// the code.
#[test]
fn help_exit0_text_on_stdout_stderr_empty() {
    let mut cmd = claude_print();
    let _env = hermetic_env(&mut cmd);
    let out = run(cmd.arg("--help"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "--help: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "--help: stdout must carry the help text\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.is_empty(),
        "--help: stderr must be empty (help is not a diagnostic), got:\n{}",
        out.stderr
    );
}

// ── --help: content pins ─────────────────────────────────────────────────────

/// The help text carries the externally documented CLI surface: the about
/// line, the exact usage line (name, `[PROMPT]` positional, `[COMMAND]`
/// subcommand), the `serve` subcommand, and the flags a drop-in `claude -p`
/// caller passes through (`--model`, `--output-format` with its three values,
/// `--allowedTools`/`--disallowedTools`, `--dangerously-skip-permissions`,
/// `--input-file`), plus both early-exit flags themselves (`-h/--help`,
/// `-V/--version`) and `--claude-binary`. No ANSI escapes: piped help is
/// plain text, so scripts grepping it cannot be broken by color codes.
#[test]
fn help_text_pins_usage_subcommands_and_compat_flags() {
    let mut cmd = claude_print();
    let _env = hermetic_env(&mut cmd);
    let out = run(cmd.arg("--help"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "--help: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    let help = &out.stdout;

    // About line is the first line, verbatim (src/cli.rs `about`).
    assert_eq!(
        help.lines().next().unwrap_or(""),
        "Drop-in replacement for `claude -p` billing against the subscription pool",
        "--help: first line must be the about text, got:\n{help}"
    );

    // Exact usage line: binary name, optional flags, optional PROMPT
    // positional, optional subcommand.
    assert!(
        help.contains("Usage: claude-print [OPTIONS] [PROMPT] [COMMAND]"),
        "--help: usage line must be pinned verbatim, got:\n{help}"
    );

    // Subcommands: the daemon mode and clap's own help subcommand.
    assert!(
        help.contains("Commands:"),
        "missing Commands section:\n{help}"
    );
    assert!(
        help.contains("serve") && help.contains("Run the pool daemon"),
        "--help: the serve subcommand must be documented:\n{help}"
    );
    assert!(
        help.contains("Print this message"),
        "--help: clap's help subcommand must be documented:\n{help}"
    );

    // Arguments: the positional prompt.
    assert!(
        help.contains("Arguments:"),
        "missing Arguments section:\n{help}"
    );
    assert!(
        help.contains("[PROMPT]"),
        "--help: the [PROMPT] positional must be documented:\n{help}"
    );

    // Drop-in compat flags plus the early-exit flags themselves.
    for needle in [
        "-f, --input-file",
        "-m, --model",
        "--max-turns",
        "-o, --output-format",
        "text, json, stream-json",
        "--allowedTools",
        "--disallowedTools",
        "--dangerously-skip-permissions",
        "--claude-binary",
        "-V, --version",
        "-h, --help",
    ] {
        assert!(
            help.contains(needle),
            "--help: help text must document `{needle}`:\n{help}"
        );
    }

    // Piped help is plain text — no terminal escapes to break greppers.
    assert!(
        !help.contains('\x1b'),
        "--help: help text must not contain ANSI escapes:\n{help}"
    );
}

// ── --help: no session, no prompt, no child ──────────────────────────────────

/// `--help` must never invoke the claude binary AT ALL. clap answers `--help`
/// inside `Cli::parse()`, before main()'s body runs — before the orphan
/// sweep, the HOME gate, the binary liveness check, and any version probe —
/// so the sentinel's invocation log must not exist. A positional prompt is
/// supplied too: it must be equally ignored (no prompt load, no session).
#[test]
fn help_never_invokes_the_claude_binary() {
    let dir = tempfile::tempdir().expect("failed to create sentinel dir");
    let sentinel = exec_sentinel(dir.path());

    let mut cmd = claude_print_sentinel(&sentinel);
    let _env = hermetic_env(&mut cmd);
    let out = run(cmd.arg("--help").arg("test prompt"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "--help: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Usage: claude-print"),
        "--help: stdout must be the help text, got:\n{}",
        out.stdout
    );
    assert!(
        sentinel_invocations(dir.path()).is_none(),
        "--help must not spawn ANY child — not a version probe, not a session \
         child — but the sentinel was invoked with:\n{}",
        sentinel_invocations(dir.path()).unwrap_or_default()
    );
}

// ── --version: output and exit code ──────────────────────────────────────────

/// `--version` (and its `-V` short form) → exit 0, stdout EXACTLY the pinned
/// single line, stderr empty. The "wrapping claude" clause comes from the
/// mock's probe answer, so this pins both halves of the line hermetically.
#[test]
fn version_exit0_exact_line_stderr_empty_and_short_v_identical() {
    let mut cmd = claude_print();
    let _env = hermetic_env(&mut cmd);
    let long = run(cmd.arg("--version"), BUDGET);

    assert_eq!(
        long.code,
        Some(0),
        "--version: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        long.code,
        long.stdout,
        long.stderr
    );
    assert_eq!(
        long.stdout,
        expected_version_line(),
        "--version: stdout must be exactly the pinned version line"
    );
    assert!(
        long.stderr.is_empty(),
        "--version: stderr must be empty, got:\n{}",
        long.stderr
    );

    // -V is the documented short form and must be byte-identical.
    let mut cmd = claude_print();
    let _env = hermetic_env(&mut cmd);
    let short = run(cmd.arg("-V"), BUDGET);
    assert_eq!(
        short.code,
        Some(0),
        "-V: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        short.code,
        short.stdout,
        short.stderr
    );
    assert_eq!(
        short.stdout, long.stdout,
        "-V and --version must print byte-identical output"
    );
}

/// A missing claude binary degrades the "wrapping claude" clause to
/// `not found` and STILL exits 0 — the probe's failure is informational, not
/// fatal. This also proves `--version` bypasses the session path's
/// `which::which` liveness gate (exit 2, "'…' not found in PATH"): dispatch
/// happens before it.
#[test]
fn version_missing_claude_binary_degrades_to_not_found_exit0() {
    let mut cmd = Command::new(workspace_bin("claude-print"));
    let _env = hermetic_env(&mut cmd);
    let out = run(
        cmd.arg("--claude-binary")
            .arg("/nonexistent/claude-binary")
            .arg("--version"),
        BUDGET,
    );

    assert_eq!(
        out.code,
        Some(0),
        "--version (missing binary): expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert_eq!(
        out.stdout,
        format!(
            "claude-print {} (wrapping claude not found)\n",
            env!("CARGO_PKG_VERSION")
        ),
        "--version (missing binary): stdout must be exactly the degraded line"
    );
}

// ── --version: no session, no prompt, exactly one probe child ────────────────

/// `--version` must invoke the claude binary EXACTLY ONCE, as the documented
/// `<bin> --version` probe — the child whose output fills the "wrapping
/// claude" clause — and never with session-shaped argv. The sentinel makes
/// the accounting total: any session child (PTY exec) or any second,
/// error-path re-probe would leave an extra `invoked:…` line; a session
/// child would also exit 97 on the PTY and fail the run. A positional prompt
/// is supplied and must be ignored.
#[test]
fn version_invokes_exactly_one_version_probe_and_no_session_child() {
    let dir = tempfile::tempdir().expect("failed to create sentinel dir");
    let sentinel = exec_sentinel(dir.path());

    let mut cmd = claude_print_sentinel(&sentinel);
    let _env = hermetic_env(&mut cmd);
    let out = run(cmd.arg("--version").arg("test prompt"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "--version: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert_eq!(
        out.stdout,
        format!(
            "claude-print {} (wrapping claude sentinel-claude 9.9.9)\n",
            env!("CARGO_PKG_VERSION")
        ),
        "--version: stdout must be exactly the pinned line with the \
         sentinel's probe answer"
    );

    // Total child accounting: exactly one invocation, and it is the probe.
    let invocations = sentinel_invocations(dir.path())
        .expect("the sentinel must have been invoked exactly once (the --version probe)");
    assert_eq!(
        invocations, "invoked:--version\n",
        "--version must spawn exactly one child, the `<bin> --version` probe \
         — no session child, no re-probe. Sentinel log:\n{invocations}"
    );
}

// ── Neither flag loads a prompt ──────────────────────────────────────────────

/// Prompt resolution must never run for either flag. Each variant supplies a
/// prompt input that the session path would reject or consume, and asserts
/// the early-exit flag still wins with its own output and exit 0:
///
///   * `--version` with NULL stdin and no positional — the session path
///     exits 4 ("no prompt provided"; pinned by `binary_e2e::no_prompt_exit4`).
///   * `--version --input-file <missing>` — the session path exits 2/4 on
///     the unresolvable input file before ever spawning a child.
///   * `--help --input-file <missing>` — same, for the help path.
///   * `--version <positional>` — the session path would run a session.
#[test]
fn early_exit_flags_never_load_a_prompt() {
    struct Variant {
        label: &'static str,
        args: Vec<String>,
        expect: Expected,
    }
    enum Expected {
        VersionLine,
        HelpText,
    }

    let variants = vec![
        Variant {
            label: "--version, null stdin, no positional (session path exits 4)",
            args: vec!["--version".to_string()],
            expect: Expected::VersionLine,
        },
        Variant {
            label: "--version --input-file <missing>",
            args: vec![
                "--version".to_string(),
                "--input-file".to_string(),
                "/nonexistent/prompt-file.txt".to_string(),
            ],
            expect: Expected::VersionLine,
        },
        Variant {
            label: "--help --input-file <missing>",
            args: vec![
                "--help".to_string(),
                "--input-file".to_string(),
                "/nonexistent/prompt-file.txt".to_string(),
            ],
            expect: Expected::HelpText,
        },
        Variant {
            label: "--version <positional prompt>",
            args: vec!["--version".to_string(), "test prompt".to_string()],
            expect: Expected::VersionLine,
        },
    ];

    for v in variants {
        let mut cmd = claude_print();
        let _env = hermetic_env(&mut cmd);
        let out = run(cmd.args(&v.args), BUDGET);

        assert_eq!(
            out.code,
            Some(0),
            "prompt never loaded ({ }): expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
            v.label,
            out.code,
            out.stdout,
            out.stderr
        );
        match v.expect {
            Expected::VersionLine => assert_eq!(
                out.stdout,
                expected_version_line(),
                "prompt never loaded ({}): stdout must be exactly the pinned \
                 version line — the prompt input was consumed instead",
                v.label
            ),
            Expected::HelpText => assert!(
                out.stdout.contains("Usage: claude-print"),
                "prompt never loaded ({}): stdout must be the help text, got:\n{}",
                v.label,
                out.stdout
            ),
        }
    }
}
