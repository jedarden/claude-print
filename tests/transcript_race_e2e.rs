//! AS-6 end-to-end test: the Stop-before-JSONL-flush race (bead bf-3isy).
//!
//! The unit tests in `tests/transcript.rs` (`test_transcript_race`,
//! `test_streaming_dedup_40_retries`) exercise `read_transcript` against
//! hand-crafted fixture files written by the test itself. They prove the retry
//! loop's *internals* but not that it absorbs the race inside a real session.
//!
//! This test drives the whole `Session::run` flow against the real `mock_claude`
//! binary (the same fixture `--check` and `tests/binary_e2e.rs` use), configured
//! with `MOCK_DELAY_JSONL=150`. Per bf-3isy, mock_claude now honors the
//! `transcript_path` it advertises: it sends the Stop FIFO payload immediately,
//! then writes the transcript JSONL file 150 ms later — reproducing the exact
//! race the 40×50 ms retry loop in `src/transcript.rs` exists to absorb. Passing
//! this test proves the retry loop works against a real delayed file write, not
//! just synthetic fixtures, and that the transcript (not the inline
//! `last_assistant_message` fallback) ends up as the source of truth.
//!
//! This is a **separate test binary** (its own process) deliberately: the MOCK_*
//! env vars it sets are process-global, and a separate binary keeps them from
//! leaking into `tests/watchdog.rs`'s `MOCK_SILENT` tests when cargo runs test
//! files in parallel within a single binary.
//!
//! The second test below, `as6_forced_retry_trace_visible_only_under_verbose`
//! (bead bf-1cwt), is the retry-visible-in-verbose half of AS-6. The first test
//! proves the retry loop *absorbs* the race; this one proves the retry is
//! *observable* — a `[claude-print <ms>ms] transcript read on attempt N`
//! (N≥2) trace lands on stderr under `--verbose` and is absent without it. It
//! runs the *compiled* `claude-print` binary as a subprocess (piped stderr, env
//! injected into the child only) rather than calling `Session::run` in-process,
//! so it captures the trace cleanly and mutates no process-global state — it is
//! parallel-safe alongside the in-process test above. The generic
//! `--verbose`-emits-a-trace guard in `tests/binary_e2e.rs` (bf-12f1) does not
//! force a retry; this test does, pinning the retry-count trace specifically.

use claude_print::cli::OutputFormat;
use claude_print::session::{LaunchOptions, Session};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Locate the mock-claude binary built alongside this test binary by `build.rs`.
///
/// Same resolution strategy as `tests/watchdog.rs` and `tests/binary_e2e.rs`:
/// the test binary lives at `target/<profile>/deps/`, the named workspace bin at
/// `target/<profile>/`.
fn mock_claude_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join("mock-claude")
}

/// RAII guard that restores an env var to its prior value on drop.
///
/// `Session::run` spawns mock_claude via `fork`+`execvp`, which inherits this
/// process's full environment, so the test sets MOCK_* vars (and redirects HOME)
/// to steer the mock. Restoring — rather than just removing — on drop keeps the
/// caller's environment intact even if a var was already set, and keeps parallel
/// tests added to this binary later from seeing leftover state.
struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvGuard {
    /// Capture the current value, then set `key` to `value`.
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prior }
    }

    /// Capture the current value, then unset `key` (defensive — clears any stale
    /// flag, e.g. MOCK_SILENT leaked from a shell env, that would derail the run).
    fn remove(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// AS-6: a Stop hook that fires 150 ms before the transcript JSONL is flushed
/// must still resolve via the retry loop.
///
/// `MOCK_DELAY_JSONL=150` makes mock_claude send the Stop FIFO payload at T=0
/// and write the transcript file at T+150 ms. `read_transcript`'s first attempt
/// (made the instant Stop is received) finds no file; the retry loop must keep
/// retrying within its 40×50 ms = 2 s budget until the write lands.
///
/// Pass criteria mirror AS-6 in the plan: `Session::run` returns `Ok`, the
/// response text is extracted correctly, and (stronger than the plan's wording)
/// we assert the text came from the transcript file rather than the inline
/// `last_assistant_message` fallback — proving the retry loop actually read the
/// delayed file, not that the test would have passed without bf-3isy.
#[test]
#[ignore]
fn as6_transcript_race_delayed_jsonl_write() {
    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!(
            "Skipping AS-6 test: mock-claude binary not found at {}",
            mock_bin.display()
        );
        return;
    }

    // Defensive: MOCK_SILENT would make the child block forever and never fire
    // Stop, turning this into a timeout. Clear it regardless of prior state.
    let _silent_guard = EnvGuard::remove("MOCK_SILENT");

    // Redirect HOME at a throwaway temp dir so mock_claude writes its transcript
    // under `$tmp/.claude/projects/<cwd-slug>/...` rather than the real `~/.claude/`
    // (which every other default-mock test already pollutes). A tempdir is a
    // valid HOME stand-in and keeps this test fully hermetic. The slug is derived
    // from cwd (mirroring real Claude Code), so it varies with the workspace path.
    let home = TempDir::new().expect("temp HOME");
    let _home_guard = EnvGuard::set("HOME", home.path().to_str().unwrap());

    // The race under test: 150 ms gap between the Stop payload and the JSONL write.
    let _delay_guard = EnvGuard::set("MOCK_DELAY_JSONL", "150");

    // Distinctive response so we can prove the text came from the transcript the
    // mock wrote, not mock_claude's hardcoded default ("Hello from mock_claude").
    const RESPONSE: &str = "as6-race-response-from-transcript";
    let _resp_guard = EnvGuard::set("MOCK_RESPONSE", RESPONSE);

    let result = Session::run(
        &mock_bin,
        // No child args needed: mock_claude derives its FIFO path from the
        // `--settings` claude-print injects and fires Stop unconditionally.
        &[],
        b"What is 2+2?".to_vec(),
        Some(30), // overall wall-clock timeout (s)
        Some(20), // PTY first-output timeout (s)
        None,     // default stream-json timeout
        Some(20), // stop-hook timeout (s)
        OutputFormat::Text,
        &LaunchOptions::default(),
    );

    let session = result.unwrap_or_else(|e| {
        panic!(
            "AS-6: Session::run should succeed despite the delayed JSONL write, got error: {e:?}"
        )
    });

    // The retry loop absorbed the race: the transcript file was read, not the
    // inline last_assistant_message fallback (which carries no token counts and
    // would have masked a broken retry loop before bf-3isy).
    assert!(
        !session.transcript.used_fallback,
        "AS-6: retry loop should read the (delayed) transcript file, not fall back to \
         last_assistant_message — used_fallback must be false"
    );
    assert_eq!(
        session.transcript.text, RESPONSE,
        "AS-6: response text must come from the transcript mock_claude wrote"
    );
    assert_eq!(
        session.transcript.num_turns, 1,
        "AS-6: mock writes exactly one assistant turn"
    );

    // Non-zero tokens prove the assistant event's usage object (all four fields,
    // stamped by mock_claude) was parsed — the fallback path yields all zeros.
    let usage = &session.transcript.usage;
    assert!(
        usage.input_tokens > 0
            && usage.output_tokens > 0
            && usage.cache_creation_input_tokens > 0
            && usage.cache_read_input_tokens > 0,
        "AS-6: all four token fields must be non-zero (read from transcript usage), got: {usage:?}"
    );

    // Sanity-check the side of the contract AS-6 doesn't directly assert: the
    // file really did land at the advertised transcript_path under the temp HOME
    // (so the mock honored its own Stop payload, and we did not touch real home).
    // The projects-dir slug is derived from THIS process's cwd — the same way
    // mock_claude (mirroring real Claude Code) and claude-print's reader do — so
    // reconstruct it with `poller::cwd_to_slug` rather than a hardcoded name.
    let cwd_slug = claude_print::poller::cwd_to_slug(
        &std::env::current_dir()
            .expect("current_dir")
            .to_string_lossy(),
    );
    let transcript_file = home
        .path()
        .join(".claude")
        .join("projects")
        .join(&cwd_slug)
        .join("mock-session-abc123.jsonl");
    assert!(
        transcript_file.exists(),
        "AS-6: mock_claude should have written the transcript at {}",
        transcript_file.display()
    );
}

// ── AS-6: retry-count trace visible only under --verbose (bf-1cwt) ──────────
//
// The in-process test above proves the retry loop *absorbs* the Stop-before-
// JSONL race. This test proves the retry is *observable* under `--verbose`
// (plan.md:127: "retry loop fires (visible in `--verbose`)"). It forces ≥1
// transcript retry with `MOCK_DELAY_JSONL=150` and asserts the retry-count
// trace `[claude-print <ms>ms] transcript read on attempt N` (N≥2) reaches
// stderr under `--verbose`, and is absent with the flag off. The generic
// verbose-trace guard in `tests/binary_e2e.rs` (bf-12f1) does NOT force a
// retry; this test pins the retry-count trace specifically.

/// Locate a workspace bin built alongside this test binary by `build.rs`.
///
/// Test binaries live at `target/<profile>/deps/`; named workspace bins live at
/// `target/<profile>/`. Same resolution strategy as `mock_claude_bin` above and
/// `tests/binary_e2e.rs`.
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
/// not exited before `budget` elapses it is killed and the test fails — so a
/// wedged mock-claude cannot hang the whole `cargo test` run. Mirrors
/// `tests/binary_e2e.rs::run`.
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

/// Per-session wall-clock budget. mock-claude responds within ~2s; the forced
/// retry adds ~150 ms; 30 s is a generous ceiling that still fails fast on a
/// wedge. Mirrors `tests/binary_e2e.rs::BUDGET`.
const BUDGET: Duration = Duration::from_secs(30);

/// Parse the `[claude-print <ms>ms] transcript read on attempt N` retry-count
/// trace, returning `N` if such a line is present. Hand-rolled rather than
/// pulling in the `regex` dev-dependency (mirrors
/// `tests/binary_e2e.rs::has_claude_print_trace`): a fixed `[claude-print `
/// prefix + ASCII digits + `ms] ` + `transcript read on attempt ` + a u64.
///
/// `N` is the 1-based read number `Tracer::trace` stamps in `src/transcript.rs`
/// — `1` means success on the first try (no retry); `N ≥ 2` means the retry loop
/// fired at least once. The "transcript retry exhausted" fallback trace does NOT
/// match (it lacks the `transcript read on attempt ` suffix).
fn transcript_retry_attempt(stderr: &str) -> Option<u64> {
    stderr.lines().find_map(|line| {
        let rest = line.strip_prefix("[claude-print ")?;
        let n_dig = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if n_dig == 0 {
            return None;
        }
        let rest = rest.get(n_dig..)?;
        let msg = rest.strip_prefix("ms] ")?;
        let num = msg.strip_prefix("transcript read on attempt ")?;
        num.trim().parse::<u64>().ok()
    })
}

/// AS-6 (retry-visible-in-verbose): a FORCED transcript retry must surface the
/// `transcript read on attempt N` (N ≥ 2) trace on stderr ONLY under `--verbose`.
///
/// `MOCK_DELAY_JSONL=150` reproduces the plan's AS-6 transcript race (plan.md
/// :126): mock-claude writes the transcript JSONL 150 ms AFTER the Stop FIFO
/// payload, so `read_transcript_traced`'s first read (made the instant Stop
/// arrives) finds no file and the 40×50 ms retry loop must keep retrying until
/// the write lands (~attempt 4 → N ≈ 4). Under `--verbose` that loop emits the
/// retry-count trace; with the flag off the tracer is a no-op so the line is
/// absent.
///
/// Env (`HOME`, `MOCK_*`) is injected into the SUBPROCESS only — never the test
/// process's global env — so this test is parallel-safe alongside the in-process
/// `as6_transcript_race_delayed_jsonl_write` test above.
#[test]
fn as6_forced_retry_trace_visible_only_under_verbose() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!(
            "Skipping AS-6 retry-trace test: built binaries missing \
             (claude-print={}, mock-claude={})",
            bin.display(),
            mock.display(),
        );
        return;
    }

    // Hermetic HOME so mock-claude's transcript write lands under a tempdir,
    // never the real ~/.claude.
    let home = TempDir::new().expect("temp HOME");
    let home_str = home.path().to_str().unwrap().to_owned();
    // Distinctive response so we could (if needed) confirm the text came from
    // the transcript the mock wrote, not its hardcoded default.
    const RESPONSE: &str = "as6-retry-trace-response";
    // plan.md:126's exact AS-6 race value: 150 ms gap between Stop and JSONL.
    const DELAY_MS: &str = "150";

    // ── Verbose run: the forced retry must be observable on stderr. ──────────
    let mut verbose = Command::new(&bin);
    verbose
        .arg("--claude-binary")
        .arg(&mock)
        .arg("--verbose")
        .arg("test prompt")
        .env("HOME", &home_str)
        .env("MOCK_DELAY_JSONL", DELAY_MS)
        .env("MOCK_RESPONSE", RESPONSE);
    let verbose_out = run(&mut verbose, BUDGET);
    assert_eq!(
        verbose_out.code,
        Some(0),
        "AS-6 (verbose): forced-retry run must exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        verbose_out.code,
        verbose_out.stdout,
        verbose_out.stderr,
    );
    let n = transcript_retry_attempt(&verbose_out.stderr).unwrap_or_else(|| {
        panic!(
            "AS-6 (verbose): stderr must contain a `transcript read on attempt N` \
             retry-count trace after a forced retry, got:\n{}",
            verbose_out.stderr,
        )
    });
    assert!(
        n >= 2,
        "AS-6 (verbose): retry-count trace must show >= 1 retry (attempt >= 2), got \
         attempt {n}\nstderr:\n{}",
        verbose_out.stderr,
    );

    // ── Same forced-retry invocation WITHOUT --verbose: tracer is a no-op. ───
    let mut quiet = Command::new(&bin);
    quiet
        .arg("--claude-binary")
        .arg(&mock)
        .arg("test prompt")
        .env("HOME", &home_str)
        .env("MOCK_DELAY_JSONL", DELAY_MS)
        .env("MOCK_RESPONSE", RESPONSE);
    let quiet_out = run(&mut quiet, BUDGET);
    assert_eq!(
        quiet_out.code,
        Some(0),
        "AS-6 (quiet): forced-retry run must exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        quiet_out.code,
        quiet_out.stdout,
        quiet_out.stderr,
    );
    assert!(
        transcript_retry_attempt(&quiet_out.stderr).is_none(),
        "AS-6 (quiet): stderr must NOT contain any `transcript read on attempt N` \
         trace without --verbose, got:\n{}",
        quiet_out.stderr,
    );
}
