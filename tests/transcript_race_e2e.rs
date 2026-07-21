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

use claude_print::cli::OutputFormat;
use claude_print::session::{LaunchOptions, Session};
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
    // under `$tmp/.claude/projects/mock-cwd/...` rather than the real `~/.claude/`
    // (which every other default-mock test already pollutes). A tempdir is a
    // valid HOME stand-in and keeps this test fully hermetic.
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
    let transcript_file = home
        .path()
        .join(".claude/projects/mock-cwd/mock-session-abc123.jsonl");
    assert!(
        transcript_file.exists(),
        "AS-6: mock_claude should have written the transcript at {}",
        transcript_file.display()
    );
}
