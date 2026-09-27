//! `--show-child-stderr` child-PTY-capture contract (bead claudepr-8b4aa030).
//!
//! `docs/notes/show-child-stderr-contract.md` is the normative definition
//! behind the compat contract's one-line summary (claude-print-local, default
//! off, `never` on the child argv). This suite holds that document against the
//! implementation in both directions:
//!
//!   * **source pins** — the `Cli` flag wired into `LaunchOptions`, both
//!     event-loop feeds, exactly six dump sites (the three exit windows × the
//!     stateless and pooled drivers), the unconditional watchdog dump versus
//!     the `!prompt_injected` guard on the other two, the `dump_to` render
//!     template and the 64 KiB cap quoted against the doc, the pooled driver
//!     taking the flag while main's inapplicability list does not name it, and
//!     the README/compat-contract cross-links;
//!   * **end to end** — the compiled binary against mock-claude: default-off
//!     prints no block on a watchdog deadline; enabled dumps the bounded
//!     block to stderr with stdout clean in text, json, and stream-json
//!     modes; a silent child stays block-free even when enabled (empty
//!     capture is a no-op); a child dying before prompt injection dumps its
//!     startup bytes (`child exited before prompt was injected`) in an A/B
//!     against the flag-off run; a pooled drive dumps the drive-time stream
//!     (the injected prompt's echo, never warmup bytes) without the stateless
//!     `sending SIGTERM` line; and an absent-socket fallback applies the flag
//!     statelessly behind the one verbose diagnostic.
//!
//! Hermetic: compiled `claude-print` + mock-claude; every run gets a throwaway
//! `HOME`, an explicit empty `--config`, and its own working directory, and
//! mock knobs travel only on the child's environment — this process never
//! mutates its own environment.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

// ── repo / doc plumbing (the `tests/pretrust_cwd_contract.rs` pattern) ──────

/// Repo root, resolved the same way as `tests/docs_test_classification.rs`:
/// an explicit override first (harnesses that copy the tree), then the runtime
/// manifest dir cargo sets for this test build.
fn repo_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_PRINT_TEST_REPO") {
        let dir = PathBuf::from(dir);
        if dir.join("Cargo.toml").exists() {
            return dir;
        }
    }
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("repo file missing at {}: {e}", path.display()))
}

fn session_source() -> String {
    read_repo_file("src/session.rs")
}

fn count_needle(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// ── source pins ──────────────────────────────────────────────────────────────

/// §1/§6: the flag exists on the CLI surface and is wired into the session's
/// launch options — the two places a regression would detach the flag from the
/// capture.
#[test]
fn the_flag_is_a_cli_field_wired_into_launch_options() {
    let cli = read_repo_file("src/cli.rs");
    assert!(
        cli.contains("#[arg(long = \"show-child-stderr\")]"),
        "--show-child-stderr must remain a defined CLI flag"
    );

    let main_rs = read_repo_file("src/main.rs");
    assert!(
        main_rs.contains("show_child_stderr: cli.show_child_stderr"),
        "main must wire cli.show_child_stderr into LaunchOptions"
    );

    let session = session_source();
    assert!(
        session.contains("pub show_child_stderr: bool"),
        "LaunchOptions must carry show_child_stderr"
    );
}

/// §2: exactly two capture feeds — one per event loop — and the capture is
/// created from the flag on both paths.
#[test]
fn both_event_loops_feed_exactly_one_capture_each() {
    let session = session_source();
    assert_eq!(
        count_needle(&session, "child_capture.feed(chunk);"),
        2,
        "the capture must be fed on exactly the stateless and pooled event loops"
    );
    assert!(
        session.contains("ChildCapture::new(launch.show_child_stderr)"),
        "the stateless driver must gate the capture on the launch option"
    );
    assert!(
        session.contains("ChildCapture::new(show_child_stderr)"),
        "the pooled driver must gate the capture on its show_child_stderr parameter"
    );
}

/// §3: exactly six dump sites — the three windows × the two drivers — with the
/// watchdog dump unconditional and the other two behind the pre-injection
/// guard.
#[test]
fn dump_sites_are_the_three_windows_on_both_paths() {
    let session = session_source();
    assert_eq!(
        count_needle(&session, "child_capture.dump("),
        6,
        "the dump must fire on exactly three exit windows per driver, no others"
    );
    // The watchdog arm: the dump hangs off `timeout_msg` — once per driver.
    assert_eq!(
        count_needle(&session, "child_capture.dump(timeout_msg);"),
        2,
        "the watchdog window must dump on both drivers"
    );
    for reason in [
        "\"child exited before prompt was injected\"",
        "\"interrupted before prompt was injected\"",
    ] {
        assert_eq!(
            count_needle(&session, reason),
            2,
            "the {reason} window must dump on both drivers"
        );
    }

    // The watchdog dump is unconditional: between `has_timeout_fired()` and
    // the Timeout return there is no `prompt_injected` gate — a mid-session
    // stall dumps even though the prompt was injected.
    for region in watchdog_timeout_regions(&session) {
        assert!(
            region.contains("child_capture.dump(timeout_msg);"),
            "the watchdog timeout arm must dump the capture"
        );
        assert!(
            !region.contains("prompt_injected"),
            "the watchdog dump must be unconditional (no prompt_injected gate): {region}"
        );
    }

    // The other two windows sit behind the pre-injection guard: each reason
    // literal is only ever passed to a dump inside `if !prompt_injected`.
    for reason in [
        "child exited before prompt was injected",
        "interrupted before prompt was injected",
    ] {
        for site in pre_injection_dump_regions(&session, reason) {
            assert!(
                site.contains("if !prompt_injected {"),
                "the {reason} dump must be guarded by !prompt_injected: {site}"
            );
        }
    }
}

/// Every `if watchdog_state.has_timeout_fired() { … }` region, so the
/// unconditional-dump claim is checked per driver rather than once globally.
fn watchdog_timeout_regions(session: &str) -> Vec<String> {
    let opener = "if watchdog_state.has_timeout_fired() {";
    let mut regions = Vec::new();
    let mut rest = session;
    while let Some(start) = rest.find(opener) {
        let after = &rest[start + opener.len()..];
        let end = after
            .find("// 14. Handle exit reason")
            .expect("watchdog arm must be followed by the exit-reason handling");
        regions.push(after[..end].to_string());
        rest = &after[end..];
    }
    assert_eq!(regions.len(), 2, "one watchdog arm per driver");
    regions
}

/// The region surrounding each pre-injection dump call site, sweeping
/// backwards to the nearest `if !prompt_injected {` (or the loop start, in
/// which case the caller's guard assertion fails).
fn pre_injection_dump_regions(session: &str, reason: &str) -> Vec<String> {
    let dump_call = format!("child_capture.dump(\"{reason}\");");
    let mut regions = Vec::new();
    let mut rest = session;
    while let Some(at) = rest.find(&dump_call) {
        let backwards = &rest[..at];
        let guard = backwards
            .rfind("if !prompt_injected {")
            .unwrap_or_else(|| panic!("{reason} dump must sit inside a !prompt_injected guard"));
        // From the guard to just past the dump call: the guard must be the
        // immediately enclosing block, not an unrelated earlier one.
        let between = &backwards[guard..];
        assert!(
            !between.contains("if prompt_injected {"),
            "unexpected intervening phase gate before the {reason} dump"
        );
        regions.push(between.to_string());
        rest = &rest[at + dump_call.len()..];
    }
    assert_eq!(regions.len(), 2, "one {reason} dump per driver");
    regions
}

/// §2/§4: the capture cap and the exact dump render template, quoted against
/// the implementation.
#[test]
fn the_render_template_and_cap_match_the_source() {
    let session = session_source();
    assert!(
        session.contains("const CAP: usize = 64 * 1024;"),
        "the capture cap must stay 64 KiB"
    );
    assert!(
        session.contains("\"claude-print: ----- child PTY output ({}, {} bytes) -----\""),
        "the dump header template must stay as documented"
    );
    assert!(
        session.contains("\"claude-print: ----- end child output -----\""),
        "the dump end marker must stay as documented"
    );
    assert!(
        session.contains("if !self.enabled || self.buf.is_empty() {"),
        "an empty or disabled capture must no-op the dump"
    );

    let doc = read_repo_file("docs/notes/show-child-stderr-contract.md");
    assert!(
        doc.contains("----- child PTY output (<reason>, <N> bytes) -----"),
        "the doc must show the dump header template"
    );
    assert!(
        doc.contains("----- end child output -----"),
        "the doc must show the end marker"
    );
    assert!(
        doc.contains("64 × 1024") || doc.contains("64 KiB"),
        "the doc must state the 64 KiB cap"
    );
    for reason in [
        "child exited before prompt was injected",
        "interrupted before prompt was injected",
    ] {
        assert!(
            doc.contains(reason),
            "the doc must carry the exact {reason} reason string"
        );
    }
}

/// §5: the pooled driver takes the flag as a session-side option while main's
/// pooled inapplicability list — the child-launch flags that cannot reach a
/// running worker — does not name it.
#[test]
fn the_pooled_dispatch_applies_rather_than_lists_the_flag() {
    let main_rs = read_repo_file("src/main.rs");

    // The inapplicability diagnostic: from its vector's construction to its
    // render. `--pretrust-cwd` is the control that proves the slice really is
    // the list.
    let start = main_rs
        .find("let mut inapplicable: Vec<&str> = Vec::new();")
        .expect("main must keep the pooled inapplicability list");
    let end = main_rs[start..]
        .find("inapplicable.join(\", \")")
        .expect("the inapplicability list must be rendered");
    let list = &main_rs[start..start + end];
    assert!(
        list.contains("--pretrust-cwd"),
        "the control proves this slice is the inapplicability list"
    );
    assert!(
        !list.contains("show_child_stderr") && !list.contains("--show-child-stderr"),
        "--show-child-stderr applies on pooled runs and must not be listed as inapplicable"
    );
}

/// §1/§3 cross-links: the README row points at this contract, and the compat
/// contract keeps its one-liner (§1 `never` row, the §3 stderr routing rule,
/// and the dedicated-contract pointer).
#[test]
fn readme_and_compat_contract_stay_linked_to_the_contract() {
    let readme = read_repo_file("README.md");
    let row_start = readme
        .find("| `--show-child-stderr` |")
        .expect("README's flag table must keep the --show-child-stderr row");
    let row_end = readme[row_start..]
        .find('\n')
        .expect("table rows are single lines");
    let row = &readme[row_start..row_start + row_end];
    assert!(
        row.contains("docs/notes/show-child-stderr-contract.md"),
        "the README row must link the contract: {row}"
    );

    let compat = read_repo_file("docs/notes/claude-p-compat-contract.md");
    assert!(
        compat.contains("| `--show-child-stderr` | — | off | never |"),
        "compat §1 must keep the flag's never-class row"
    );
    assert!(
        compat.contains("`--show-child-stderr` dumps"),
        "compat §3's stderr routing rule must keep naming the dumps"
    );
    assert!(
        compat.contains("docs/notes/show-child-stderr-contract.md"),
        "compat §1 must point readers at the dedicated contract"
    );
}

// ── binary-e2e plumbing ──────────────────────────────────────────────────────

/// Locate a workspace bin built alongside this test binary (`current_exe()` →
/// `target/<profile>/deps/` → `target/<profile>/`), so the suite never names a
/// written-out build path.
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/")
        .join(name)
}

/// Per-run wall-clock budget: a stalled run ends ~4 s in (dialog dismissal,
/// prompt injection, the 2 s deadline); 60 s is a generous ceiling that still
/// fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(60);

/// One test's hermetic world: a throwaway `HOME` (transcripts and the config
/// probe land here), an explicit empty `--config`, and a private working
/// directory.
struct World {
    home: TempDir,
    workdir: TempDir,
    config: PathBuf,
}

impl World {
    fn new() -> Self {
        let home = TempDir::new().expect("temp home");
        let workdir = TempDir::new().expect("temp workdir");
        let config = home.path().join("config.toml");
        fs::write(&config, "").expect("write empty config");
        Self {
            home,
            workdir,
            config,
        }
    }

    /// A `claude-print` command in this world: hermetic HOME + workdir; the
    /// caller adds mock knobs (child env) and flags.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(workspace_bin("claude-print"));
        cmd.current_dir(self.workdir.path());
        cmd.env("HOME", self.home.path());
        cmd
    }

    /// The hermetic baseline flags: mock backend and the explicit empty
    /// config, ahead of whatever else the caller appends.
    fn args(&self, cmd: &mut Command) {
        cmd.arg("--claude-binary").arg(workspace_bin("mock-claude"));
        cmd.arg("--config").arg(&self.config);
    }
}

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `cmd` to completion with a null stdin; kill (and fail) past `budget`.
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
                    panic!("claude-print did not exit within {budget:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };
    let output = child.wait_with_output().expect("wait_with_output");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// ── dump-block assertion helpers ─────────────────────────────────────────────

/// The stderr needles every dump carries.
const HEADER_NEEDLE: &str = "claude-print: ----- child PTY output (";
const END_NEEDLE: &str = "claude-print: ----- end child output -----";
/// The mock's standard trust-dialog first line — the child bytes a stalled
/// startup run has captured by deadline time.
const DIALOG_NEEDLE: &str = "Do you trust and Allow access to this folder?";
/// The stop-hook deadline's description prefix (the watchdog window's reason).
const STOP_HOOK_REASON: &str = "Stop hook did not fire within deadline after prompt injection";

/// Exactly one dump block, and the header's byte count equals the captured
/// tail's length (± the normalization LF §4 describes).
fn assert_one_dump_block(stderr: &str, reason_needle: &str) {
    assert_eq!(
        stderr.matches(HEADER_NEEDLE).count(),
        1,
        "at most one dump per invocation — stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(reason_needle),
        "the dump header must carry the expected reason — stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(END_NEEDLE),
        "the dump block must be closed — stderr:\n{stderr}"
    );

    // Byte-count accounting: `<N> bytes) -----` in the header vs the bytes
    // between the header line and the end-marker line.
    let header_at = stderr.find(HEADER_NEEDLE).expect("header present");
    let count_at = stderr[header_at..]
        .find(" bytes) -----")
        .expect("header carries a byte count");
    let digits: String = stderr[header_at..header_at + count_at]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let n: usize = digits.parse().expect("header byte count is a number");

    let tail_start = header_at + stderr[header_at..].find('\n').expect("header line ends");
    let end_at = stderr.find(END_NEEDLE).expect("end marker present");
    // Bytes between the header line's LF and the end marker: the tail itself,
    // plus the header's own LF, plus at most the one normalization LF §4
    // describes — so the block spans n+1 or n+2 bytes, never anything else.
    let tail_len = end_at - tail_start;
    assert!(
        tail_len == n + 1 || tail_len == n + 2,
        "the header's byte count ({n}) must match the block's payload span \
         ({tail_len} = tail + 1..2 LFs) — stderr:\n{stderr}"
    );
}

/// The stdout payload stream must never carry dump bytes (§4).
fn assert_stdout_clean_of_dumps(outcome: &Outcome) {
    assert!(
        !outcome.stdout.contains(HEADER_NEEDLE) && !outcome.stdout.contains(END_NEEDLE),
        "stdout must never carry dump bytes — stdout:\n{}",
        outcome.stdout
    );
}

// ── end to end: the stall scenario (watchdog window) ─────────────────────────

/// A stateless run held past the stop-hook deadline: the mock renders its
/// trust dialog (captured), the scanner dismisses it and injects the prompt,
/// and `MOCK_DELAY_STOP` then holds the Stop payload past
/// `--stop-hook-timeout`. Deterministic mid-session stall, prompt injected.
fn stalled_run(show_child_stderr: bool, output_format: Option<&str>) -> Outcome {
    let world = World::new();
    let mut cmd = world.cmd();
    cmd.env("MOCK_DELAY_STOP", "8000");
    world.args(&mut cmd);
    cmd.args(["--stop-hook-timeout", "2"]);
    if let Some(format) = output_format {
        cmd.args(["--output-format", format]);
    }
    if show_child_stderr {
        cmd.arg("--show-child-stderr");
    }
    cmd.arg("a prompt that will stall");
    run(&mut cmd, BUDGET)
}

/// §1/§4: default off — the deadline diagnostics appear, the block does not,
/// stdout stays empty.
#[test]
fn default_off_prints_no_child_output_block_on_a_watchdog_deadline() {
    let out = stalled_run(false, None);
    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);
    assert!(
        out.stderr.contains(STOP_HOOK_REASON),
        "the deadline diagnostic must still fire — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("sending SIGTERM to child pid"),
        "the stateless teardown line must still fire — stderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains(HEADER_NEEDLE) && !out.stderr.contains(END_NEEDLE),
        "default off must print no dump block — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.is_empty(),
        "text mode keeps stdout empty — stdout:\n{}",
        out.stdout
    );
}

/// §4, text mode: enabled — the block lands on stderr between the deadline
/// diagnostics and the error line, carrying the captured dialog bytes, and
/// stdout stays empty.
#[test]
fn enabled_text_mode_dumps_the_captured_tail_to_stderr() {
    let out = stalled_run(true, None);
    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);
    assert_one_dump_block(&out.stderr, STOP_HOOK_REASON);
    assert!(
        out.stderr.contains(DIALOG_NEEDLE),
        "the dump must carry the child's captured startup bytes — stderr:\n{}",
        out.stderr
    );

    // Ordering (§4): the deadline diagnostics precede the block, the mode-
    // shaped error report follows it.
    let deadline_at = out.stderr.find(STOP_HOOK_REASON).expect("deadline line");
    let header_at = out.stderr.find(HEADER_NEEDLE).expect("header");
    let error_at = out
        .stderr
        .find("error: operation timed out")
        .expect("error line");
    assert!(
        deadline_at < header_at && header_at < error_at,
        "the block sits between the diagnostics and the error report — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.is_empty(),
        "text mode keeps stdout empty — stdout:\n{}",
        out.stdout
    );
}

/// §4, json mode: the dump stays on stderr while stdout carries exactly the
/// one-line structured error object — a JSON caller's stream is never
/// interleaved with dump bytes.
#[test]
fn enabled_json_mode_keeps_the_dump_on_stderr_and_the_error_on_stdout() {
    let out = stalled_run(true, Some("json"));
    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);
    assert_one_dump_block(&out.stderr, STOP_HOOK_REASON);
    assert_stdout_clean_of_dumps(&out);

    let lines: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "json stdout must be exactly one line — stdout:\n{}",
        out.stdout
    );
    let value: serde_json::Value = serde_json::from_str(lines[0]).expect("json stdout parses");
    assert_eq!(value["type"], "result");
    assert_eq!(value["subtype"], "timeout");
    assert_eq!(value["is_error"], true);
}

/// §4, stream-json mode: the dump stays on stderr while the synthesized error
/// result streams on stdout (the prompt was injected, so the after-inject
/// routing applies).
#[test]
fn enabled_stream_json_mode_dumps_to_stderr_while_the_error_object_streams() {
    let out = stalled_run(true, Some("stream-json"));
    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);
    assert_one_dump_block(&out.stderr, STOP_HOOK_REASON);
    assert_stdout_clean_of_dumps(&out);

    let lines: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert!(
        !lines.is_empty(),
        "stream-json stdout must carry the synthesized error result — stdout:\n{}",
        out.stdout
    );
    // Every line is valid JSON (the stream contract) and exactly one carries
    // the synthesized timeout result — no dump bytes, no interleaving.
    let results: Vec<serde_json::Value> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("stream-json line {i} is not valid JSON: {e}\n{line}"))
        })
        .filter(|v| v["type"] == "result")
        .collect();
    assert_eq!(
        results.len(),
        1,
        "exactly one result object — stdout:\n{}",
        out.stdout
    );
    assert_eq!(results[0]["subtype"], "timeout");
    assert_eq!(results[0]["is_error"], true);
}

/// §3: an empty capture dumps nothing even when enabled — the silent-child
/// wedge produces the deadline diagnostics but no block.
#[test]
fn enabled_but_silent_child_dumps_no_block() {
    let world = World::new();
    let mut cmd = world.cmd();
    cmd.env("MOCK_SILENT", "1");
    world.args(&mut cmd);
    cmd.args(["--first-output-timeout", "2", "--show-child-stderr"]);
    cmd.arg("a prompt against a silent child");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);
    assert!(
        out.stderr
            .contains("child produced no PTY output within deadline"),
        "the first-output deadline diagnostic must fire — stderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains(HEADER_NEEDLE) && !out.stderr.contains(END_NEEDLE),
        "an empty capture must no-op the dump — stderr:\n{}",
        out.stderr
    );
}

// ── end to end: pre-injection child death ────────────────────────────────────

/// §3/§4: a child that renders its trust dialog and then exits (before any
/// prompt could be injected) dumps its startup bytes under the
/// `child exited before prompt was injected` reason — and the identical
/// flag-off run prints no block. Both exit 2 with the same error line.
#[test]
fn child_death_before_prompt_injection_dumps_the_startup_bytes() {
    for show_child_stderr in [true, false] {
        let world = World::new();
        let mut cmd = world.cmd();
        cmd.env("MOCK_EXIT_BEFORE_STOP", "1");
        world.args(&mut cmd);
        if show_child_stderr {
            cmd.arg("--show-child-stderr");
        }
        cmd.arg("a prompt against a dying child");
        let out = run(&mut cmd, BUDGET);

        assert_eq!(
            out.code,
            Some(2),
            "flag={show_child_stderr} — stderr:\n{}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("error: claude exited before Stop hook fired"),
            "flag={show_child_stderr} — stderr:\n{}",
            out.stderr
        );
        if show_child_stderr {
            assert_one_dump_block(&out.stderr, "child exited before prompt was injected");
            assert!(
                out.stderr.contains(DIALOG_NEEDLE),
                "the dump must carry the dialog the child rendered before dying — stderr:\n{}",
                out.stderr
            );
        } else {
            assert!(
                !out.stderr.contains(HEADER_NEEDLE),
                "flag off must print no block — stderr:\n{}",
                out.stderr
            );
        }
    }
}

// ── end to end: pooled ───────────────────────────────────────────────────────

/// Ceiling for a daemon warmup to reach `settled and ready` (the mock
/// completes trust dialog → dismissal → idle-settle in well under a second;
/// the ceiling only absorbs a loaded box).
const WARMUP: Duration = Duration::from_secs(90);

/// Ceiling for the daemon's worker count to settle after a drive.
const REPLACE: Duration = Duration::from_secs(30);

/// A running `claude-print serve --pool-size 1` daemon with its stderr
/// collected line-by-line on a reader thread (the `tests/pool_socket_e2e.rs`
/// harness). Dropping kills the daemon so a failing assertion cannot leak
/// worker processes into the rest of the suite.
struct Daemon {
    child: std::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// Start over `socket_path` with this fixture's HOME, forwarding
    /// `extra_env` to every worker it spawns.
    fn start(socket_path: &Path, home: &Path, extra_env: &[(&str, &str)]) -> Daemon {
        let mut cmd = Command::new(workspace_bin("claude-print"));
        cmd.arg("--claude-binary")
            .arg(workspace_bin("mock-claude"))
            .arg("serve")
            .args(["--pool-size", "1"])
            .arg("--socket")
            .arg(socket_path)
            .arg("--verbose")
            .env("HOME", home);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn claude-print serve");

        let stderr = child.stderr.take().expect("stderr piped");
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let reader = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => sink.lock().unwrap().push(line),
                    Err(_) => break,
                }
            }
        });
        Daemon {
            child,
            lines,
            reader: Some(reader),
        }
    }

    /// Block until stderr contains `needle` at least `count` times.
    fn wait_for(&mut self, needle: &str, count: usize, budget: Duration) {
        let start = Instant::now();
        loop {
            let hits = self
                .lines
                .lock()
                .unwrap()
                .iter()
                .filter(|l| l.contains(needle))
                .count();
            if hits >= count {
                return;
            }
            assert!(
                start.elapsed() < budget,
                "timed out waiting for {count:?} occurrences of {needle:?}; stderr: {:?}",
                self.lines.lock().unwrap()
            );
            if let Ok(Some(_)) = self.child.try_wait() {
                panic!("daemon exited while waiting for {needle:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Parse the parent PID out of the contents of `/proc/<pid>/stat` (the comm
/// field may contain spaces, so scanning starts after its closing `)`).
fn stat_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Every live process whose parent is `ppid` — while the daemon lives this is
/// exactly its worker set (serve mode forks nothing else).
fn children_of(ppid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
            if stat_ppid(&stat) == Some(ppid) {
                found.push(pid);
            }
        }
    }
    found
}

/// §5: the flag applies on pooled runs — a daemon-driven worker held past the
/// stop-hook deadline dumps the drive-time stream (the injected prompt's
/// echo, never the warmup bytes the daemon observed before the client
/// existed) with the deadline reason and no `sending SIGTERM` line.
#[test]
fn pooled_run_applies_the_flag_and_dumps_the_drive_time_stream() {
    let config = tempfile::tempdir().expect("config tempdir");
    fs::create_dir_all(config.path().join("claude-print")).expect("config dir");
    fs::write(config.path().join("claude-print/config.toml"), "").expect("empty config");
    let home = tempfile::tempdir().expect("home tempdir");
    let socket = config.path().join("pool.sock");

    let mut daemon = Daemon::start(
        &socket,
        home.path(),
        &[("MOCK_DELAY_STOP", "8000"), ("MOCK_UNIQUE_SESSION_ID", "1")],
    );
    daemon.wait_for("settled and ready", 1, WARMUP);

    let prompt = "pooled stall prompt for the echo";
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg(workspace_bin("mock-claude"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", config.path())
        .arg("--pool-socket")
        .arg(&socket)
        .arg("--verbose")
        .arg("--show-child-stderr")
        .args(["--stop-hook-timeout", "2"])
        .arg(prompt);
    let out = run(&mut cmd, BUDGET);
    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);

    // It really was a pooled drive, not a quiet fallback.
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the verbose trace must prove the pooled dispatch — stderr:\n{}",
        out.stderr
    );
    assert_one_dump_block(&out.stderr, STOP_HOOK_REASON);

    // The captured tail is the client's invocation window: the prompt's PTY
    // echo is in, the daemon's warmup dialog is not.
    assert!(
        out.stderr.contains(prompt),
        "the dump must carry the drive-time echo of the injected prompt — stderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains(DIALOG_NEEDLE) && !out.stderr.contains("Welcome to Claude Code"),
        "the dump must not carry the daemon's warmup bytes — stderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("sending SIGTERM to child pid"),
        "the pooled timeout must not claim a direct child signal — stderr:\n{}",
        out.stderr
    );

    // The wedged worker is released on return; let the daemon settle back to
    // exactly one (replacement) worker before the bounded shutdown.
    let daemon_pid = daemon.child.id();
    let start = Instant::now();
    loop {
        let workers = children_of(daemon_pid);
        if workers.len() == 1 {
            break;
        }
        assert!(
            start.elapsed() < REPLACE,
            "the daemon must settle back to one worker after the timed-out drive; \
             workers now: {workers:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(daemon);
}

// ── end to end: stateless fallback ───────────────────────────────────────────

/// §5: an absent socket falls back statelessly behind exactly one verbose
/// diagnostic, and the flag applies to the fallback's own fresh child — full
/// stateless semantics, SIGTERM line included.
#[test]
fn fallback_run_applies_the_flag_statelessly() {
    let world = World::new();
    let missing_socket = world.home.path().join("no-such-pool.sock");
    let mut cmd = world.cmd();
    cmd.env("MOCK_DELAY_STOP", "8000");
    world.args(&mut cmd);
    cmd.arg("--pool-socket").arg(&missing_socket);
    cmd.arg("--verbose");
    cmd.arg("--show-child-stderr");
    cmd.args(["--stop-hook-timeout", "2"]);
    cmd.arg("a fallback stall prompt");
    let out = run(&mut cmd, BUDGET);

    assert_eq!(out.code, Some(124), "stderr:\n{}", out.stderr);

    let pool_lines: Vec<&str> = out.stderr.lines().filter(|l| l.contains("pool:")).collect();
    assert_eq!(
        pool_lines.len(),
        1,
        "exactly one fallback diagnostic — stderr:\n{}",
        out.stderr
    );
    assert!(
        pool_lines[0].contains("falling back"),
        "the diagnostic must name the fallback: {}",
        pool_lines[0]
    );
    assert!(
        !out.stderr.contains("driving prewarmed worker"),
        "the fallback must not claim a pooled worker — stderr:\n{}",
        out.stderr
    );

    // Full stateless semantics: the fresh child was captured, and the
    // stateless teardown line is present.
    assert_one_dump_block(&out.stderr, STOP_HOOK_REASON);
    assert!(
        out.stderr.contains(DIALOG_NEEDLE),
        "the dump must carry the fallback run's own startup bytes — stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("sending SIGTERM to child pid"),
        "the fallback is a stateless session — stderr:\n{}",
        out.stderr
    );
}
