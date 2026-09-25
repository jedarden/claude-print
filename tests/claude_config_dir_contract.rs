//! Regression contract for preserving Claude's real config directory
//! (bead claudepr-bfe97ce4).
//!
//! AGENTS.md "Key invariants" #1 and plan.md HR-4 / ADR-001: claude-print
//! must not set `CLAUDE_CONFIG_DIR`, so the child's transcripts land in the
//! REAL config dir `$HOME/.claude/projects/` — the only tree claude-print
//! reads (the poller's derivation fallback and the stream-json live reader
//! are both HOME-rooted). Until this bead the invariant had no direct test:
//! nothing asserted that the variable is absent from the child environment —
//! in particular that an *inherited* value is removed, not merely that
//! claude-print declines to add one — nor that a run's transcript actually
//! lands under `$HOME/.claude/projects`.
//!
//! The inherited half matters because the leak is real: outer wrappers
//! (agent cleanrooms, NEEDLE marathon sandboxes) export CLAUDE_CONFIG_DIR to
//! relocate Claude Code's whole config dir, and everything they spawn
//! inherits it. A leaked value redirects the child's transcript root while
//! claude-print keeps watching `$HOME/.claude/projects` — the same
//! inherited-env class as the session markers (tests/nested_session.rs).
//!
//! Coverage, three layers:
//!   * `pty_spawner_child_env_has_no_claude_config_dir` — a real child
//!     spawned through the public `PtySpawner` API receives no
//!     `CLAUDE_CONFIG_DIR` (hermetic: no process-env mutation, no claude).
//!   * `scrubbed_env_lists_claude_config_dir` — source wiring: the scrub
//!     lives in `SCRUBBED_ENV` in `src/pty.rs`, the one env builder the exec
//!     path uses (the pure-function halves, injected-vs-inherited, are the
//!     `scrub_env_*` unit tests in that file).
//!   * Binary end-to-end — the compiled `claude-print` runs against
//!     mock-claude with an inherited decoy `CLAUDE_CONFIG_DIR` and a
//!     throwaway `HOME`. The child env dump (`MOCK_RECORD_ENV`) must show no
//!     `CLAUDE_CONFIG_DIR`, and the transcript must land under
//!     `<HOME>/.claude/projects/` — never under the decoy — with the session
//!     reading it back successfully (json mode: non-null `session_id`, real
//!     usage). mock-claude models real claude's redirect behavior (honors
//!     `CLAUDE_CONFIG_DIR` when present), so a scrub regression fails these
//!     assertions instead of passing vacuously.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Per-run wall-clock budget. mock-claude completes a session in ~2s; 30s is
/// a generous ceiling that still fails fast on a wedge (same calibration as
/// tests/binary_e2e.rs).
const BUDGET: Duration = Duration::from_secs(30);

/// Locate a workspace bin built alongside this test binary.
///
/// Test binaries live at `target/<profile>/deps/`; named workspace bins live
/// at `target/<profile>/`. Same resolution strategy as tests/binary_e2e.rs.
/// `mock-claude` is a bin target of this package, so any `cargo test` run —
/// plain or filtered — links both binaries into `target/<profile>/`.
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

// ── Layer 1: child environment through the public PTY API ───────────────────

/// A child spawned via `PtySpawner` (the production exec path) must not
/// receive `CLAUDE_CONFIG_DIR`. `env` prints its own environment to the PTY.
///
/// This mutates no process environment, so it cannot race the parallel test
/// threads in this binary (see the scrub_env doc comment in pty.rs for why
/// env mutation in tests is avoided). The assertion holds whether or not the
/// test runner itself carries `CLAUDE_CONFIG_DIR` — a runner-side value must
/// be scrubbed just like a wrapper-inherited one.
#[test]
fn pty_spawner_child_env_has_no_claude_config_dir() {
    use claude_print::pty::PtySpawner;
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let cmd = CString::new("env").unwrap();
    let spawner = PtySpawner::spawn(&cmd, &[]).expect("PtySpawner::spawn should succeed");

    let master_fd = spawner.master.as_raw_fd();
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // SAFETY: master_fd is a valid PTY master fd owned by `spawner`.
        let n = unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break; // EOF, or EIO once the child exits and closes the slave side
        }
        output.extend_from_slice(&buf[..n as usize]);
    }
    let _ = nix::sys::wait::waitpid(spawner.child_pid, None);

    let text = String::from_utf8_lossy(&output);

    // Sanity: this really is the env-built child (the forced variables
    // arrive through the same builder that must drop CLAUDE_CONFIG_DIR).
    assert!(
        text.contains("CLAUDE_CODE_ENTRYPOINT=cli"),
        "sanity: child env must carry the forced CLAUDE_CODE_ENTRYPOINT=cli, got: {text:?}"
    );

    // The invariant: the config-dir variable must not reach the child —
    // whether the runner had one set (an inherited redirect to scrub) or not
    // (claude-print must not inject one; ADR-001's rejected sandbox design).
    assert!(
        !text.contains("CLAUDE_CONFIG_DIR="),
        "invariant 1: CLAUDE_CONFIG_DIR must not reach the child env, got: {text:?}"
    );
}

// ── Layer 2: source wiring ───────────────────────────────────────────────────

/// The scrub must live in `SCRUBBED_ENV` in `src/pty.rs` — the one list the
/// pre-fork env builder (`build_child_env` → `execvpe`) filters through. A
/// redirect removed anywhere else would not affect the exec'd child.
#[test]
fn scrubbed_env_lists_claude_config_dir() {
    let pty_source = include_str!("../src/pty.rs");
    // Scope the search to the const block itself: a bare file-wide `contains`
    // is satisfied by this bead's own unit-test fixtures in the same file
    // (`("CLAUDE_CONFIG_DIR", …)`), so removing the real entry would not
    // fail it — exactly the mutation this canary exists to catch.
    let scrubbed_block = pty_source
        .split("const SCRUBBED_ENV")
        .nth(1)
        .expect("src/pty.rs must still define SCRUBBED_ENV")
        .split("];")
        .next()
        .expect("the SCRUBBED_ENV block must be terminated");
    assert!(
        scrubbed_block.contains("\"CLAUDE_CONFIG_DIR\","),
        "invariant 1 is enforced in SCRUBBED_ENV (src/pty.rs): an inherited \
         CLAUDE_CONFIG_DIR must be scrubbed before execvpe so transcripts land \
         in the real config dir $HOME/.claude/projects"
    );
}

// ── Layer 3: binary end-to-end with an inherited decoy ─────────────────────

/// A captured claude-print subprocess outcome.
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// One hermetic binary run: compiled claude-print → mock-claude, with a
/// throwaway `HOME`, a decoy `CLAUDE_CONFIG_DIR` inherited from the "wrapper"
/// environment, `XDG_CONFIG_HOME` redirected into the sandbox, and
/// `MOCK_RECORD_ENV` capturing the environment the session child actually
/// received. `home`/`decoy`/`_scratch` are returned (not dropped) because the
/// caller inspects transcript placement under both roots and reads the env
/// dump after the run — dropping the scratch TempDir early would delete
/// `child-env.bin` before the assertion gets to it.
struct HermeticRun {
    outcome: Outcome,
    home: TempDir,
    decoy: TempDir,
    _scratch: TempDir,
    env_dump: PathBuf,
}

fn run_with_decoy_config_dir(output_format: &str, response: &str) -> HermeticRun {
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

    let home = TempDir::new().expect("throwaway HOME");
    let decoy = TempDir::new().expect("decoy CLAUDE_CONFIG_DIR root");
    let xdg = TempDir::new().expect("throwaway XDG_CONFIG_HOME");
    let scratch = TempDir::new().expect("scratch dir for the child env dump");
    let env_dump = scratch.path().join("child-env.bin");

    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary")
        .arg(&mock)
        .arg("--output-format")
        .arg(output_format)
        .arg("--timeout")
        .arg("25")
        .arg("preserve the real config directory")
        // The "outer wrapper" environment claude-print is spawned under. Every
        // value rides Command::env, not the test runner's process env.
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", xdg.path())
        .env("CLAUDE_CONFIG_DIR", decoy.path())
        .env("MOCK_RESPONSE", response)
        .env("MOCK_RECORD_ENV", &env_dump)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if start.elapsed() >= BUDGET {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit within {BUDGET:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }
    };
    let output = child.wait_with_output().expect("wait_with_output");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    HermeticRun {
        outcome: Outcome {
            code: code.or(output.status.code()),
            stdout,
            stderr,
        },
        home,
        decoy,
        _scratch: scratch,
        env_dump,
    }
}

/// Parse a `MOCK_RECORD_ENV` dump: NUL-separated KEY=VALUE entries.
fn dumped_env_entries(path: &Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "mock-claude never wrote the env dump at {}: {e} — was the session child spawned?",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

/// Every `*.jsonl` file under `root`, recursively (an absent dir has none).
fn find_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                    found.push(path);
                }
            }
        }
    }
    found
}

/// The session child's environment, as recorded by mock-claude, must carry no
/// `CLAUDE_CONFIG_DIR` even though claude-print itself was spawned with one —
/// the inherited redirect must be gone by exec time.
#[test]
fn binary_child_env_drops_inherited_claude_config_dir() {
    let run = run_with_decoy_config_dir("text", "config dir env contract reply");

    assert_eq!(
        run.outcome.code,
        Some(0),
        "the session must succeed under an inherited decoy CLAUDE_CONFIG_DIR\n\
         stdout:\n{}\nstderr:\n{}",
        run.outcome.stdout,
        run.outcome.stderr
    );
    assert!(
        run.outcome.stdout.contains("config dir env contract reply"),
        "stdout must carry the transcript-backed response, got:\n{}",
        run.outcome.stdout
    );

    let entries = dumped_env_entries(&run.env_dump);

    // Sanity: the dump is the session child's env, post-claude-print-builder
    // (the forced billing variable arrives through the same builder).
    assert!(
        entries.iter().any(|e| e == "CLAUDE_CODE_ENTRYPOINT=cli"),
        "sanity: the forced CLAUDE_CODE_ENTRYPOINT=cli must be in the child env dump"
    );

    assert!(
        !entries.iter().any(|e| e.starts_with("CLAUDE_CONFIG_DIR=")),
        "invariant 1: an inherited CLAUDE_CONFIG_DIR must be scrubbed from the \
         child env; dump:\n{entries:?}"
    );
}

/// Transcript placement: with an inherited decoy `CLAUDE_CONFIG_DIR`, the
/// transcript must still land under the (throwaway) HOME's real config dir —
/// `.claude/projects/` — and nowhere under the decoy, and claude-print must
/// read it back (json mode: non-null session_id, transcript-sourced usage).
/// mock-claude honors `CLAUDE_CONFIG_DIR` when present, so a scrub regression
/// relocates the transcript to the decoy and fails this test.
#[test]
fn binary_transcript_lands_in_real_config_dir_despite_inherited_decoy() {
    let run = run_with_decoy_config_dir("json", "real config dir placement reply");

    assert_eq!(
        run.outcome.code,
        Some(0),
        "the session must succeed under an inherited decoy CLAUDE_CONFIG_DIR\n\
         stdout:\n{}\nstderr:\n{}",
        run.outcome.stdout,
        run.outcome.stderr
    );

    let result: serde_json::Value = serde_json::from_str(run.outcome.stdout.trim())
        .unwrap_or_else(|e| panic!("json stdout must parse: {e}\n{}", run.outcome.stdout));
    assert_eq!(
        result.get("is_error"),
        Some(&serde_json::Value::Bool(false)),
        "run must succeed: {result}"
    );
    assert!(
        result
            .get("session_id")
            .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty())),
        "session_id must be a non-empty string (transcript was found and read): {result}"
    );
    // Transcript-sourced usage: mock-claude's transcript carries the fixed
    // 10/25/5/15 token fields, so a populated usage object proves the JSONL
    // under $HOME/.claude/projects was read back, not just written.
    let usage = result
        .get("usage")
        .unwrap_or_else(|| panic!("usage object must be present: {result}"));
    assert_eq!(
        usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64),
        Some(10),
        "usage must come from the transcript the session read: {result}"
    );

    // Placement: exactly one transcript JSONL, under HOME's real config dir.
    let real_projects = run.home.path().join(".claude").join("projects");
    let under_home = find_jsonl(&real_projects);
    assert_eq!(
        under_home.len(),
        1,
        "exactly one transcript must land under {} (found {under_home:?})",
        real_projects.display()
    );
    let transcript = std::fs::read_to_string(&under_home[0])
        .expect("the transcript under HOME's real config dir must be readable");
    assert!(
        transcript.contains("real config dir placement reply"),
        "the HOME transcript must carry this session's response:\n{transcript}"
    );

    // And nothing under the decoy: the inherited redirect never took effect.
    let under_decoy = find_jsonl(run.decoy.path());
    assert!(
        under_decoy.is_empty(),
        "invariant 1: no transcript may land under the decoy CLAUDE_CONFIG_DIR \
         (found {under_decoy:?}) — the inherited redirect reached the child"
    );
}
