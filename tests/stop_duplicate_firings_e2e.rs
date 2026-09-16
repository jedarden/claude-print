//! End-to-end regression test: duplicate/extra Stop firings on degraded runs
//! (bead claudepr-8dcf53ce).
//!
//! `docs/notes/hook-design.md` ("Stop Firing Frequency") documents that
//! permission-denied tool runs can produce an extra Stop firing, and that the
//! single-fire poller degrades gracefully: it acts on the first payload, the
//! transcript retry/fallback path absorbs the rest, and the watchdog still
//! bounds the session. Before this bead, nothing tested that tolerance — the
//! only Stop-timing e2e coverage (`tests/watchdog.rs`) exercises a child that
//! NEVER fires Stop.
//!
//! These tests drive real sessions against `mock_claude` with
//! `MOCK_EXTRA_STOPS=2`: after the normal payload + transcript sequence the
//! mock re-fires Stop twice more — first a byte-identical duplicate of the
//! real payload, then a spurious phantom turn (different session_id, a
//! transcript_path that is never written, different last_assistant_message).
//! Whether the extra payloads coalesce into the poller's first FIFO read or
//! land after it has returned and are discarded at cleanup, the session must
//! emit exactly one result, exit cleanly, and neither corrupt the output with
//! the later firings' content nor emit it twice.
//!
//! The deterministic coalesced shape (all payloads in the buffer before the
//! read) is pinned separately at the poller level by
//! `tests/stop_poller.rs::test_extra_stop_firings_first_payload_wins_single_fire`;
//! these e2e tests assert the interleaving-agnostic invariants of full
//! sessions. This is a separate test binary for the same reason as
//! `tests/transcript_race_e2e.rs`: the in-process test's MOCK_* env vars are
//! process-global, and a separate binary keeps them away from other suites'
//! children under parallel `cargo test`.

use claude_print::cli::OutputFormat;
use claude_print::session::{LaunchOptions, Session};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// Serializes the in-process test's env mutations (the `EnvGuard` set/remove
// below) so a second env-touching test added to this binary later cannot race
// one mid-flight. Same pattern as tests/home_unset.rs, tests/watchdog.rs, and
// tests/transcript_race_e2e.rs. The subprocess-based test needs no lock: it
// injects env into the child only.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Locate a workspace bin built alongside this test binary by the workspace build.
///
/// Same resolution strategy as `tests/transcript_race_e2e.rs` and
/// `tests/binary_e2e.rs`: the test binary lives at `target/<profile>/deps/`,
/// the named workspace bin at `target/<profile>/`.
fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// RAII guard that restores an env var to its prior value on drop.
///
/// `Session::run` spawns mock_claude via `fork`+`execvp`, which inherits this
/// process's full environment, so the in-process test sets MOCK_* vars (and
/// redirects HOME) to steer the mock. Restoring — rather than just removing —
/// on drop keeps the caller's environment intact even if a var was already
/// set, and keeps parallel tests added to this binary later from seeing
/// leftover state.
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

    /// Capture the current value, then unset `key` (defensive — clears any
    /// stale flag, e.g. MOCK_SILENT leaked from a shell env, that would make
    /// the child block forever and turn this into a timeout).
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

/// Count `claude-print-*` run directories under `dir` — the per-run temp dirs
/// `HookInstaller` creates and must remove on every exit path (AGENTS.md
/// invariant 2). Counting inside a private TMPDIR (rather than the shared
/// `/tmp` the watchdog tests poll) keeps foreign test binaries' concurrent
/// runs out of the count entirely.
fn count_claude_print_temp_dirs(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
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
        })
        .unwrap_or(0)
}

/// Distinctive first-payload response. Any path that acted on a later firing
/// instead would surface either the spurious `last_assistant_message`
/// (fallback text) or mock_claude's hardcoded default — not this string.
const RESPONSE: &str = "dup-stop-first-payload-response";

/// Degraded-run duplication at the session level: `MOCK_EXTRA_STOPS=2` makes
/// mock_claude re-fire Stop (duplicate, then spurious) after the normal
/// payload + transcript sequence. `Session::run` must still return exactly one
/// clean result built from the FIRST payload.
#[test]
fn duplicate_stop_firings_session_emits_single_clean_result() {
    let _lock = env_lock();

    let mock_bin = workspace_bin("mock-claude");
    if !mock_bin.exists() {
        eprintln!(
            "Skipping test: mock-claude binary not found at {}",
            mock_bin.display()
        );
        return;
    }

    // Defensive: MOCK_SILENT would make the child block forever and never fire
    // Stop, turning this into a timeout test instead.
    let _silent_guard = EnvGuard::remove("MOCK_SILENT");

    // Hermetic HOME so mock_claude's transcript write lands under a tempdir,
    // never the real ~/.claude (same pattern as the AS-6 in-process test).
    let home = TempDir::new().expect("temp HOME");
    let _home_guard = EnvGuard::set("HOME", home.path().to_str().unwrap());

    // Private TMPDIR for the same hermeticity reason at the artifact level:
    // HookInstaller's per-run dir is created (and must be removed) here, where
    // no other test binary's runs can perturb the count below.
    let run_tmp = TempDir::new().expect("temp TMPDIR");
    let _tmp_guard = EnvGuard::set("TMPDIR", run_tmp.path().to_str().unwrap());

    let _resp_guard = EnvGuard::set("MOCK_RESPONSE", RESPONSE);
    // The degraded run: first payload complete, then one duplicate re-fire and
    // one spurious phantom turn.
    let _extra_guard = EnvGuard::set("MOCK_EXTRA_STOPS", "2");

    assert_eq!(
        count_claude_print_temp_dirs(run_tmp.path()),
        0,
        "private TMPDIR must start with no claude-print run dirs"
    );

    let result = Session::run(
        &mock_bin,
        &[],
        b"What is 2+2?".to_vec(),
        Some(30), // overall wall-clock timeout (s)
        Some(20), // PTY first-output timeout (s)
        None,     // default stream-json timeout
        Some(20), // stop-hook timeout (s)
        OutputFormat::Text,
        &LaunchOptions::default(),
    );

    // Clean exit: the extra firings must not turn the run into an error
    // (EC-7 backstop, parse failure, timeout, or child-exit race).
    let session = result.unwrap_or_else(|e| {
        panic!("duplicate Stop firings must still produce a clean session, got error: {e:?}")
    });

    // Exactly one result, from the first payload: the text is the first
    // response VERBATIM — a double-emit concatenated onto itself, a spurious
    // firing's fallback text, or any corruption of the buffer fails here.
    assert_eq!(
        session.transcript.text, RESPONSE,
        "response text must be exactly the first payload's answer, once"
    );
    assert_eq!(
        session.transcript.session_id.as_deref(),
        Some("mock-session-abc123"),
        "session id must come from the first payload, not the spurious firing"
    );
    assert_eq!(
        session.transcript.num_turns, 1,
        "exactly one turn — later firings must not add turns or results"
    );
    // The transcript file (not the last_assistant_message fallback) must be
    // the source of truth. If a later firing had been acted on, its advertised
    // transcript_path never exists and the retry loop would exhaust into the
    // fallback — used_fallback=true — failing this assertion loudly.
    assert!(
        !session.transcript.used_fallback,
        "first payload's transcript must be read; fallback means a later firing won"
    );

    // Clean exit, artifact side: the run dir must be gone on the success path
    // (AGENTS.md invariant 2). Retry briefly — cleanup is Drop-based and the
    // filesystem can lag a tick behind.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if count_claude_print_temp_dirs(run_tmp.path()) == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "temp run dir must be cleaned up after a clean exit despite extra Stop firings"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ── Binary level: exactly one result emitted on stdout ──────────────────────

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

/// Per-session wall-clock budget. mock-claude responds within ~2s; 30s is a
/// generous ceiling that still fails fast on a wedge. Mirrors
/// `tests/binary_e2e.rs::BUDGET`.
const BUDGET: Duration = Duration::from_secs(30);

/// Degraded-run duplication at the binary level: with `MOCK_EXTRA_STOPS=2` the
/// compiled `claude-print` must exit 0 and emit the result EXACTLY once —
/// stdout carries the first response a single time, with no spurious content
/// (text mode) and exactly one result object carrying the first payload's
/// session id (json mode). Env (`HOME`, `MOCK_*`) is injected into the
/// SUBPROCESS only, so this test is parallel-safe alongside the in-process
/// test above.
#[test]
fn duplicate_stop_firings_binary_emits_exactly_one_result() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!(
            "Skipping test: built binaries missing (claude-print={}, mock-claude={})",
            bin.display(),
            mock.display(),
        );
        return;
    }

    // Hermetic HOME (mock transcript writes) and TMPDIR (per-run artifacts).
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    // ── Text mode: the response appears exactly once on stdout. ─────────────
    let mut text = Command::new(&bin);
    text.arg("--claude-binary")
        .arg(&mock)
        .arg("test prompt")
        .env("HOME", home.path())
        .env("TMPDIR", run_tmp.path())
        .env("MOCK_EXTRA_STOPS", "2")
        .env("MOCK_RESPONSE", RESPONSE);
    let out = run(&mut text, BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "text mode: exit must be clean (0) despite extra Stop firings\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr,
    );
    assert_eq!(
        out.stdout.trim(),
        RESPONSE,
        "text mode: stdout must be exactly the first response"
    );
    assert_eq!(
        out.stdout.matches(RESPONSE).count(),
        1,
        "text mode: the response must be emitted exactly once — a double-emit \
         regression repeats it"
    );
    assert!(
        !out.stdout.contains("spurious"),
        "text mode: no spurious firing content may leak into stdout:\n{}",
        out.stdout
    );

    // ── JSON mode: exactly one result line, built from the first payload. ───
    let mut json = Command::new(&bin);
    json.arg("--claude-binary")
        .arg(&mock)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt")
        .env("HOME", home.path())
        .env("TMPDIR", run_tmp.path())
        .env("MOCK_EXTRA_STOPS", "2")
        .env("MOCK_RESPONSE", RESPONSE);
    let out = run(&mut json, BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "json mode: exit must be clean (0) despite extra Stop firings\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr,
    );

    let lines: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "json mode: exactly one result object must be emitted — a double-emit \
         regression produces more (or an unparseable concatenation). stdout:\n{}",
        out.stdout
    );
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap_or_else(|e| {
        panic!(
            "json mode: result line must be valid JSON: {e}\nraw:\n{}",
            lines[0]
        )
    });
    assert_eq!(v["type"], "result", "json mode: type must be 'result'");
    assert_eq!(
        v["subtype"], "success",
        "json mode: subtype must be 'success' (clean exit)"
    );
    assert_eq!(
        v["is_error"], false,
        "json mode: is_error must be false (clean exit)"
    );
    assert_eq!(
        v["session_id"], "mock-session-abc123",
        "json mode: session id must come from the first payload, not a later firing"
    );
    assert_eq!(
        v["result"], RESPONSE,
        "json mode: result must be the first payload's response, once"
    );
    assert_eq!(
        v["num_turns"], 1,
        "json mode: exactly one turn — later firings must not add results"
    );
}
