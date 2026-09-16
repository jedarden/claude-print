//! Regression test for the transcript retry window between Stop-hook fire and
//! the final JSONL flush (bead claudepr-31c4acf5).
//!
//! `docs/research/claude-code-internals.md` §"Race Condition: Stop Hook Fires
//! Before JSONL Flush" documents a 2–5 ms window where, at Stop-hook fire time,
//! the final `assistant` event is either **not yet written** or the last chunk
//! is **partially written (truncated JSON line)**. `docs/notes/claude-contract-
//! probes.md` (OQ-1 resolution and the single-turn Stop contract) relies on
//! `read_transcript`'s 40×50 ms retry loop plus the `last_assistant_message`
//! fallback to absorb that window; plan PO-5 pins the same mitigation.
//!
//! Existing coverage did not pin the path end-to-end:
//!
//! * `tests/transcript.rs::test_transcript_race` / `test_streaming_dedup_40_retries`
//!   simulate the whole FILE being absent, then appearing — not a transcript
//!   that exists, parses, and merely lacks its final assistant line.
//! * `tests/transcript_race_e2e.rs::as6_transcript_race_delayed_jsonl_write`
//!   covers the same file-absent shape through a real session — but is
//!   `#[ignore]`d and passes no `last_assistant_message`, so nothing proved a
//!   present fallback is SUPPRESSED when the transcript catches up.
//! * No test asserted the retry loop is BOUNDED (gives up in finite time
//!   instead of spinning), nor that all three output formats carry the
//!   complete final message once the flush lands.
//!
//! The tests below use a fixture transcript whose final assistant line is
//! absent (or truncated) on the first reads and present on retry — the exact
//! documented flush window — and assert:
//!
//! 1. `text` and `json` emitter output carry the complete final message
//!    (test 1), and the `stream-json` reader forwards the flushed line
//!    (test 3);
//! 2. the retry loop is bounded — it keeps retrying until the flush lands
//!    (elapsed ≥ the append delay, proving a retry actually happened) and
//!    gives up within a hard ceiling when the transcript never catches up
//!    (test 4);
//! 3. the documented `last_assistant_message` fallback fires ONLY when the
//!    transcript never catches up: a decoy fallback supplied while the
//!    transcript catches up is ignored (`used_fallback == false`, transcript
//!    text wins — test 1); the real fallback fires only after the retry
//!    budget is exhausted (`used_fallback == true`, elapsed within the
//!    bounded budget — test 4).
use claude_print::cli::OutputFormat;
use claude_print::emitter::{emit_success, spawn_stream_json_reader_to};
use claude_print::transcript::read_transcript;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Distinctive decoy fallback passed as `last_assistant_message` in the
/// catch-up tests. If the retry loop were broken (falling back before the
/// transcript catches up), the assertions on the transcript text would fail
/// with this string visible in the diff.
const DECOY_FALLBACK: &str = "DECOY last_assistant_message fallback — transcript must win";

/// The complete final assistant message the flush lands at T+150 ms.
const FINAL_TEXT: &str = "final assistant message flushed after Stop";

/// How long after the first read the final assistant line is appended,
/// simulating the flush landing inside the retry window. Comfortably above
/// the real 2–5 ms window and the suite's other 100–150 ms race fixtures,
/// far inside the 40×50 ms = 2 s retry budget.
const FLUSH_DELAY_MS: u64 = 150;

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Arc<Mutex<Vec<u8>>>, CaptureWriter) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = CaptureWriter(Arc::clone(&buf));
    (buf, writer)
}

/// A pre-flush transcript record: a user turn that references the session.
/// Real PTY transcripts carry ordinary records like this BEFORE the final
/// assistant event, so the file exists, parses cleanly, and yields a
/// `sessionId` — but no assistant text — while the flush window is open.
fn user_event(session_id: &str, text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "sessionId": session_id
    })
    .to_string()
}

/// The final assistant event whose flush the race window delays.
fn final_assistant_event() -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-flush-window",
            "content": [{"type": "text", "text": FINAL_TEXT}],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 7,
                "cache_creation_input_tokens": 3,
                "cache_read_input_tokens": 4
            }
        },
        "sessionId": "sess-flush-window"
    })
    .to_string()
}

/// Create the transcript pre-flush: only the user record is on disk, exactly
/// as claude leaves it during the 2–5 ms window before the final assistant
/// event is flushed. Returns (tempdir, path, pre-flush byte size).
fn pre_flush_transcript() -> (TempDir, std::path::PathBuf, u64) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("flush-window.jsonl");
    let user_line = format!("{}\n", user_event("sess-flush-window", "what is 2+2?"));
    std::fs::write(&path, &user_line).unwrap();
    let pre_flush_len = user_line.len() as u64;
    (dir, path, pre_flush_len)
}

/// Append `line` (+ newline) to the transcript after `delay_ms`, the way
/// claude's flush lands after the Stop hook fires.
fn append_after(path: std::path::PathBuf, line: String, delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", line).unwrap();
    });
}

// ── 1. Final line absent on first read, present on retry: transcript wins ────

/// Core flush-window regression: the transcript exists and parses on the first
/// read but carries no assistant text yet (the final event is unflushed); the
/// final assistant line lands 150 ms later. The retry loop must absorb the
/// window, and the result — in `text` and `json` emitter output — must carry
/// the COMPLETE final message from the transcript, never the decoy
/// `last_assistant_message` supplied alongside it.
#[test]
fn flush_window_final_line_absent_then_present_transcript_wins_over_decoy_fallback() {
    let _dir_guard;
    let (dir, path, _pre_len) = pre_flush_transcript();
    _dir_guard = dir; // bind to keep the tempdir alive for the whole test
    append_after(path.clone(), final_assistant_event(), FLUSH_DELAY_MS);

    let start = Instant::now();
    let r = read_transcript(&path, Some(DECOY_FALLBACK)).unwrap();
    let elapsed = start.elapsed();

    // The transcript caught up, so the documented fallback must NOT be used.
    assert!(
        !r.used_fallback,
        "fallback must be used only when the transcript never catches up; \
         the transcript caught up here, so used_fallback must be false"
    );
    assert_eq!(
        r.text, FINAL_TEXT,
        "retry loop must return the COMPLETE final message from the flushed \
         transcript, not the decoy fallback or a partial read"
    );
    assert_ne!(
        r.text, DECOY_FALLBACK,
        "the decoy last_assistant_message must never surface when the \
         transcript catches up"
    );
    // Causality: the final line does not exist until the T+150 ms append, so a
    // success at all — let alone with the full text — proves the loop retried
    // past the flush window instead of giving up on the first read.
    assert!(
        elapsed >= Duration::from_millis(FLUSH_DELAY_MS - 50),
        "read_transcript must have retried until the flush landed (append at \
         T+{FLUSH_DELAY_MS} ms), but returned after only {elapsed:?}"
    );
    // The successful parse still sees the pre-flush record: full-session data.
    assert_eq!(r.session_id.as_deref(), Some("sess-flush-window"));
    assert_eq!(r.num_turns, 1);
    assert_eq!(r.usage.input_tokens, 12);
    assert_eq!(r.usage.output_tokens, 7);

    // ── text output carries the complete final message ──
    let (text_buf, mut text_writer) = capture();
    emit_success(&mut text_writer, &r, &OutputFormat::Text, "2.1.270", 1234).unwrap();
    let text_out = String::from_utf8(text_buf.lock().unwrap().clone()).unwrap();
    assert_eq!(
        text_out,
        format!("{FINAL_TEXT}\n"),
        "text output must contain the complete final message"
    );

    // ── json output carries the complete final message ──
    let (json_buf, mut json_writer) = capture();
    emit_success(&mut json_writer, &r, &OutputFormat::Json, "2.1.270", 1234).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&json_buf.lock().unwrap().clone()).unwrap();
    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    assert_eq!(
        v["result"], FINAL_TEXT,
        "json output must carry the complete final message, got: {v}"
    );
    assert_eq!(v["num_turns"], 1);
    assert_eq!(v["usage"]["input_tokens"], 12);
}

// ── 2. Truncated final line on first reads, completed on retry ───────────────

/// The documented race has a second manifestation: "the last chunk may be
/// partially written (truncated JSON line)". The fixture writes a truncated
/// fragment of the final assistant line (no newline) at T+100 ms, then
/// completes it at T+250 ms. Intermediate reads must skip the malformed
/// fragment (empty text → keep retrying) and the completed line must surface
/// whole once the flush finishes — never the truncated prefix, never the
/// decoy fallback.
#[test]
fn flush_window_truncated_final_line_completed_on_retry() {
    let _dir_guard;
    let (dir, path, _pre_len) = pre_flush_transcript();
    _dir_guard = dir;

    let full_line = final_assistant_event();
    // Split the final event mid-structure: a valid-looking start that is not
    // parseable JSON on its own, mirroring a line caught mid-write.
    let split_at = full_line.find("\"content\"").unwrap();
    let fragment = full_line[..split_at].to_string();
    let remainder = full_line[split_at..].to_string();

    let p2 = path.clone();
    std::thread::spawn(move || {
        // Partial write: the fragment lands WITHOUT a trailing newline.
        std::thread::sleep(Duration::from_millis(100));
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&p2).unwrap();
            write!(f, "{fragment}").unwrap();
        }
        // The flush completes: the rest of the line plus the newline.
        std::thread::sleep(Duration::from_millis(150));
        let mut f = std::fs::OpenOptions::new().append(true).open(&p2).unwrap();
        writeln!(f, "{remainder}").unwrap();
    });

    let start = Instant::now();
    let r = read_transcript(&path, Some(DECOY_FALLBACK)).unwrap();
    let elapsed = start.elapsed();

    assert!(
        !r.used_fallback,
        "transcript caught up; fallback must not fire"
    );
    assert_eq!(
        r.text, FINAL_TEXT,
        "the completed line must surface whole — the truncated prefix must be \
         skipped by the intermediate reads and retried past"
    );
    // The line is not parseable until T+250 ms; success proves bounded retries
    // spanned the truncated window.
    assert!(
        elapsed >= Duration::from_millis(200),
        "read_transcript must have retried past the truncated-write window \
         (line complete at T+250 ms), but returned after only {elapsed:?}"
    );
}

// ── 3. stream-json reader forwards the final line once flushed ───────────────

/// In `--output-format stream-json` the transcript reaches stdout through the
/// live reader thread (`spawn_stream_json_reader_to`), which tails the file
/// from its injection-time size — NOT through `emit_success`. The same flush
/// window applies: the reader is already tailing when Stop fires, and the
/// final assistant line must be forwarded verbatim once the flush lands.
/// Modeled faithfully: the reader starts at the pre-flush byte offset (the
/// "existed at injection and has grown" discovery case), and only the
/// post-offset bytes — the flushed final line — may appear in its output.
#[test]
fn flush_window_stream_json_reader_forwards_final_line_once_flushed() {
    let _dir_guard;
    let (dir, path, pre_flush_len) = pre_flush_transcript();
    _dir_guard = dir;

    let (buf, writer) = capture();
    let handle = spawn_stream_json_reader_to(path.clone(), pre_flush_len, Box::new(writer));

    append_after(path.clone(), final_assistant_event(), FLUSH_DELAY_MS);

    // Give the live tail time to observe the flushed line (5 ms poll loop;
    // 400 ms is comfortable) before draining.
    std::thread::sleep(Duration::from_millis(400));
    handle.signal_drain();
    drop(handle);

    let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "the reader starts at the pre-flush offset, so exactly one line — the \
         flushed final assistant event — must be forwarded; got:\n{out}"
    );
    assert!(
        lines[0].contains(FINAL_TEXT),
        "stream-json output must contain the complete final message once the \
         flush lands; got: {lines:?}"
    );
    assert!(
        lines[0].contains("\"type\":\"assistant\""),
        "the forwarded line must be the full assistant JSONL event, verbatim; \
         got: {lines:?}"
    );
}

// ── 4. Fallback fires ONLY after the bounded retries are exhausted ───────────

/// The other side of PO-5: when the transcript NEVER catches up (Stop fires
/// more than the whole 40×50 ms = 2 s budget ahead of the flush — the doc's
/// "use Stop hook payload fallback" branch), the documented
/// `last_assistant_message` fallback must fire, but only after the full retry
/// budget is spent. The fixture is a transcript that stays assistant-less
/// forever (plus a permanently truncated last line — the doc's second
/// manifestation, persisting this time). Boundedness is asserted by wall
/// clock: at least most of the 2 s budget must elapse before the fallback
/// (proving the loop retried rather than falling back immediately), and the
/// call must RETURN inside a hard ceiling (proving the loop is bounded — a
/// regression to an unbounded retry would hang past the ceiling).
#[test]
fn fallback_used_only_after_transcript_never_catches_up_within_bounded_retries() {
    let _dir_guard;
    let (dir, path, _pre_len) = pre_flush_transcript();
    _dir_guard = dir;
    // A truncated final line that NEVER completes — the transcript never
    // catches up in any form. Must be skipped on every read, never panic.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, "{{\"type\":\"assistant\",\"message\":{{\"id\":\"msg-cut").unwrap();
    }

    const FALLBACK: &str = "fallback response after retry exhaustion";
    let start = Instant::now();
    let r = read_transcript(&path, Some(FALLBACK)).unwrap();
    let elapsed = start.elapsed();

    assert!(
        r.used_fallback,
        "the transcript never catches up, so the documented \
         last_assistant_message fallback must be used"
    );
    assert_eq!(r.text, FALLBACK);
    // Fallback semantics: no transcript-derived turn counts or token usage.
    assert_eq!(r.num_turns, 0);
    assert_eq!(
        r.usage,
        claude_print::transcript::AggregatedUsage::default()
    );

    // Bounded, two-sided:
    //  - LOWER: the retry budget is actually spent before falling back. The
    //    loop performs 41 reads separated by 40×50 ms sleeps, so it cannot
    //    legitimately return much before 2 s; anything faster would mean the
    //    fallback fired before the transcript had its chance to catch up.
    //    (thread::sleep guarantees at-least semantics, so the floor is safe.)
    //  - UPPER: the loop terminates. A regression to unbounded retries would
    //    blow past this ceiling and fail the test instead of hanging CI.
    assert!(
        elapsed >= Duration::from_millis(1500),
        "fallback fired after only {elapsed:?} — the full bounded retry budget \
         (40×50 ms ≈ 2 s) must be exhausted before last_assistant_message is \
         consulted"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "retries must be bounded: read_transcript returned after {elapsed:?}, \
         dangerously close to unbounded behavior"
    );
}
