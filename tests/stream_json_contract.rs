//! Golden compatibility tests for the stream-json output contract.
//!
//! The contract these fixtures pin is specified in
//! `docs/notes/stream-json-contract.md`; each test names the section it pins.
//! Golden fixtures are version-pinned like the transcript captures
//! (`tests/fixtures/transcript_v2.1.*.jsonl`): the `v2.1.270` in the filename
//! is the pinned Claude Code version from
//! `docs/notes/claude-contract-probes.md`, and the record shapes are modeled
//! on those captures.
//!
//! What the golden pairs pin, and why each file exists:
//!
//! * `stream_json_golden_v2.1.270.input.jsonl` — a PTY-shaped transcript (the
//!   shape `claude-print` actually tails: `summary`/`user`/`assistant`/
//!   `system` records, NO `result` record) carrying one instance of every
//!   byte-level case the contract calls out: compact JSON, spaced JSON (no
//!   re-serialization), unicode text, a `thinking` block, split assistant
//!   records sharing a `message.id` (forwarded verbatim, no dedup), a blank
//!   line (dropped), and a CRLF-terminated line (CR trimmed).
//! * `stream_json_golden_v2.1.270.expected.jsonl` — the exact stdout bytes a
//!   full replay must produce: input minus the blank line, CR removed, every
//!   line LF-terminated.
//! * `stream_json_golden_v2.1.270.errors.jsonl` — the exact bytes of the two
//!   synthesized `result` error objects: the session-error line written to
//!   stdout (line 1) and the config-error line written to stderr (line 2).
//!
//! Byte-level guarantees (verbatim forwarding, ordering, no duplication, no
//! filtering by record type) are pinned by comparing the reader's stdout
//! against the expected fixture EXACTLY — any re-serialization, reorder,
//! drop, or duplicate shows up as a byte diff to a committed file, visible in
//! review.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claude_print::cli::OutputFormat;
use claude_print::emitter::{
    emit_error, spawn_stream_json_reader_bound_to, spawn_stream_json_reader_to,
};
use claude_print::error::ClaudePrintError;

const INPUT: &str = include_str!("fixtures/stream_json_golden_v2.1.270.input.jsonl");
const EXPECTED: &str = include_str!("fixtures/stream_json_golden_v2.1.270.expected.jsonl");
const ERRORS: &str = include_str!("fixtures/stream_json_golden_v2.1.270.errors.jsonl");
const CAPTURE_V233: &str = include_str!("fixtures/transcript_v2.1.233.jsonl");

/// The claude version stamped into the golden error objects — the pinned
/// version from `docs/notes/claude-contract-probes.md`, in the same
/// `<x.y.z> (Claude Code)` form `resolve_claude_version` records.
const GOLDEN_CLAUDE_VERSION: &str = "2.1.270 (Claude Code)";

// ── helpers ──────────────────────────────────────────────────────────────────

/// One forwarded chunk: the instant its `write` landed plus the bytes.
type Chunk = (Instant, Vec<u8>);

/// Collects everything the reader forwards (one entry per `write` call, with
/// the instant it arrived) so both byte content and arrival order/timing can
/// be asserted.
#[derive(Clone)]
struct ChunkLogWriter {
    chunks: Arc<Mutex<Vec<Chunk>>>,
}

impl ChunkLogWriter {
    fn new() -> Self {
        Self {
            chunks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn total_len(&self) -> usize {
        self.chunks
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.len())
            .sum()
    }

    fn concatenated(&self) -> Vec<u8> {
        let chunks = self.chunks.lock().unwrap();
        let mut out = Vec::with_capacity(chunks.iter().map(|(_, b)| b.len()).sum());
        for (_, bytes) in chunks.iter() {
            out.extend_from_slice(bytes);
        }
        out
    }
}

impl Write for ChunkLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.chunks
            .lock()
            .unwrap()
            .push((Instant::now(), buf.to_vec()));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Byte offset just past the `n`-th LF-terminated line (the snapshot-size
/// semantics of `pre_existing`: a byte count, not a line count).
fn byte_offset_after_lines(bytes: &[u8], n: usize) -> usize {
    let mut seen = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            seen += 1;
            if seen == n {
                return i + 1;
            }
        }
    }
    bytes.len()
}

/// The expected bytes for a replay that starts at the snapshot offset after
/// the first `n` input lines: the first `n` lines of the expected fixture
/// correspond to the first `n` input lines (the blank input line is line 8),
/// so skipping is line-aligned between the two files.
fn expected_from_line(n: usize) -> Vec<u8> {
    EXPECTED.as_bytes()[byte_offset_after_lines(EXPECTED.as_bytes(), n)..].to_vec()
}

/// The two golden error lines as LF-terminated byte strings
/// (`(stdout_session_error, stderr_config_error)`).
fn golden_error_lines() -> (Vec<u8>, Vec<u8>) {
    let split = byte_offset_after_lines(ERRORS.as_bytes(), 1);
    (
        ERRORS.as_bytes()[..split].to_vec(),
        ERRORS.as_bytes()[split..].to_vec(),
    )
}

/// Poll until the writer has accumulated at least `min_len` bytes, so tests
/// never sleep-past-the-fact on forwarding speed.
fn wait_for_bytes(writer: &ChunkLogWriter, min_len: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while writer.total_len() < min_len {
        assert!(
            Instant::now() < deadline,
            "reader forwarded {} of {} expected bytes within 5s — got: {:?}",
            writer.total_len(),
            min_len,
            String::from_utf8_lossy(&writer.concatenated()),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Materialize `bytes` as the session transcript inside a fresh projects dir;
/// returns `(temp dir, transcript path, identity path)`.
fn staged_transcript(dir: &Path, bytes: &[u8]) -> (PathBuf, PathBuf, PathBuf) {
    let projects_dir = dir.join("projects").join("gold-cwd");
    std::fs::create_dir_all(&projects_dir).unwrap();
    let transcript = projects_dir.join("gold-session-270.jsonl");
    std::fs::write(&transcript, bytes).unwrap();
    let identity = dir.join("session-identity.json");
    std::fs::write(
        &identity,
        format!(
            r#"{{"session_id":"gold-session-270","transcript_path":"{}","cwd":"/gold/cwd"}}"#,
            transcript.display(),
        ),
    )
    .unwrap();
    (dir.to_path_buf(), transcript, identity)
}

// ── §4 + §5 of the contract: verbatim full replay, ordering, no duplication ──

/// A full replay of the golden transcript must reach stdout byte-for-byte
/// identical to the expected fixture — and a Stop-payload retarget naming the
/// path the reader is already bound to must not duplicate or reset it (the
/// normal Stop transition does exactly this before draining).
#[test]
fn golden_full_replay_is_byte_identical_and_retarget_does_not_duplicate() {
    let dir = tempfile::TempDir::new().unwrap();
    let (_root, transcript, _identity) = staged_transcript(dir.path(), INPUT.as_bytes());

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_to(
        transcript.clone(),
        0,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
    );

    wait_for_bytes(&writer, EXPECTED.len());

    // The normal Stop transition: retarget to the payload's transcript path
    // (here: the path already bound), then drain. Golden equality below fails
    // if either the retarget duplicates lines or the drain truncates them.
    handle.retarget(transcript.clone());
    handle.signal_drain();
    drop(handle);

    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        EXPECTED,
        "stream-json stdout diverged from the golden expected bytes"
    );
    assert!(
        !writer.concatenated().contains(&b'\r'),
        "a carriage return reached stdout — CR trimming is part of the framing contract"
    );
}

/// The same golden bytes must come out of the production binding path:
/// identity-file binding with an injection-time snapshot offset forwarding
/// only post-injection records (§7 Transcript identity binding).
#[test]
fn golden_identity_binding_with_snapshot_offset_skips_pre_injection_lines() {
    let dir = tempfile::TempDir::new().unwrap();
    let (_root, transcript, identity) = staged_transcript(dir.path(), INPUT.as_bytes());

    // The injection snapshot saw the first 3 input records already on disk
    // (summary, user prompt, first assistant record) — their bytes are
    // pre-injection and must be skipped; everything after them is forwarded.
    let pre_existing = std::collections::HashMap::from([(
        transcript.clone(),
        byte_offset_after_lines(INPUT.as_bytes(), 3) as u64,
    )]);

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_bound_to(
        identity,
        transcript.parent().unwrap().to_path_buf(),
        pre_existing,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
        // No watchdog flag: this test pins the §7 binding contract, not the
        // Phase-2 first-output credit (claudepr-33fdf4ed pins that).
        None,
    );

    wait_for_bytes(&writer, expected_from_line(3).len());
    handle.signal_drain();
    drop(handle);

    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        String::from_utf8_lossy(&expected_from_line(3)),
        "snapshot-offset replay diverged from the golden expected bytes after line 3"
    );
}

// ── §6 of the contract: incremental delivery while the transcript grows ─────

/// Forwarding is LIVE: with the reader running and the transcript growing
/// line by line, lines reach stdout before the drain is signaled, in append
/// order, and the accumulated output is at every point a prefix of the golden
/// expected bytes (the chunk log replays into exactly the expected bytes, in
/// arrival order).
#[test]
fn golden_incremental_arrival_replays_expected_bytes_in_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let projects_dir = dir.path().join("projects").join("gold-cwd");
    std::fs::create_dir_all(&projects_dir).unwrap();
    let transcript = projects_dir.join("gold-session-270.jsonl");
    std::fs::write(&transcript, b"").unwrap();

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_to(
        transcript.clone(),
        0,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
    );

    // Grow the transcript the way a child does: one record at a time, in
    // append order, blank line and CRLF line included.
    let mut file = File::options().append(true).open(&transcript).unwrap();
    for line in INPUT.split_inclusive('\n') {
        file.write_all(line.as_bytes()).unwrap();
        file.flush().unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    let drain_at = Instant::now();
    handle.signal_drain();
    drop(handle);

    let arrived_before_drain: usize = writer
        .chunks
        .lock()
        .unwrap()
        .iter()
        .filter(|(at, _)| *at < drain_at)
        .map(|(_, b)| b.len())
        .sum();

    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        EXPECTED,
        "incremental replay diverged from the golden expected bytes"
    );
    assert!(
        arrived_before_drain >= byte_offset_after_lines(EXPECTED.as_bytes(), 3),
        "only {arrived_before_drain} of {} expected bytes arrived before the drain was \
         signaled — forwarding was not incremental",
        EXPECTED.len(),
    );
}

// ── §3 of the contract: the print/SDK-shaped capture (result record present) ─

/// The committed v2.1.233 transcript capture is print/SDK-shaped — it ends in
/// a `result` record. It must be forwarded like any other record, verbatim:
/// the reader neither filters it out nor synthesizes one for PTY-shaped
/// transcripts that lack it.
#[test]
fn golden_v2_1_233_capture_is_forwarded_verbatim_including_result_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let (_root, transcript, _identity) = staged_transcript(dir.path(), CAPTURE_V233.as_bytes());

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_to(
        transcript,
        0,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
    );
    wait_for_bytes(&writer, CAPTURE_V233.len());
    handle.signal_drain();
    drop(handle);

    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        CAPTURE_V233,
        "the v2.1.233 capture did not round-trip byte-for-byte"
    );
}

// ── §2 of the contract: framing edge cases ───────────────────────────────────

/// A transcript whose final record lacks a trailing newline still produces a
/// newline-terminated final stdout line (§2 Wire format).
#[test]
fn golden_final_record_without_trailing_newline_is_still_terminated() {
    let dir = tempfile::TempDir::new().unwrap();
    let unterminated = &INPUT.as_bytes()[..INPUT.len() - 1];
    let (_root, transcript, _identity) = staged_transcript(dir.path(), unterminated);

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_to(
        transcript,
        0,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
    );
    wait_for_bytes(&writer, EXPECTED.len());
    handle.signal_drain();
    drop(handle);

    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        EXPECTED,
        "an unterminated final record must still yield the exact golden bytes"
    );
}

/// The reader does not parse or validate the records it forwards — a line
/// that is not valid JSON is forwarded verbatim like any other (§3 Event
/// schema: forwarding is type- and validity-agnostic; parsing is the
/// consumer's job).
#[test]
fn golden_non_json_lines_are_forwarded_verbatim() {
    let dir = tempfile::TempDir::new().unwrap();
    let mut bytes = INPUT.as_bytes().to_vec();
    bytes.extend_from_slice(b"this is not json at all\n");

    let (_root, transcript, _identity) = staged_transcript(dir.path(), &bytes);

    let writer = ChunkLogWriter::new();
    let handle = spawn_stream_json_reader_to(
        transcript,
        0,
        Box::new(writer.clone()) as Box<dyn Write + Send>,
    );
    wait_for_bytes(&writer, EXPECTED.len() + b"this is not json at all\n".len());
    handle.signal_drain();
    drop(handle);

    let mut expected = EXPECTED.as_bytes().to_vec();
    expected.extend_from_slice(b"this is not json at all\n");
    assert_eq!(
        String::from_utf8_lossy(&writer.concatenated()),
        String::from_utf8_lossy(&expected),
        "a non-JSON line must be forwarded verbatim, not filtered"
    );
}

// ── §8 of the contract: synthesized error results ────────────────────────────

/// The synthesized `result` error objects are byte-golden: a session error in
/// stream-json mode writes its result line to STDOUT (after the streamed
/// lines), a config error writes its result line to STDERR with stdout left
/// untouched. Exit codes and subtypes are pinned by `src/error.rs` unit
/// tests; this pins the wire bytes.
#[test]
fn golden_synthesized_error_result_bytes() {
    let (session_error_line, config_error_line) = golden_error_lines();

    // Session error (e.g. watchdog timeout): stdout, after the streamed lines.
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    emit_error(
        &mut stdout,
        &mut stderr,
        &ClaudePrintError::Timeout,
        &OutputFormat::StreamJson,
        GOLDEN_CLAUDE_VERSION,
        true,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&session_error_line),
        "session-error result bytes diverged from the golden error line"
    );
    assert!(stderr.is_empty(), "session errors must not write to stderr");

    // Config error: stderr, stdout empty — before a session exists, stdout,
    // which carries the response payload, is left clean.
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    emit_error(
        &mut stdout,
        &mut stderr,
        &ClaudePrintError::Config(
            "invalid config: bad.toml: unsupported key `frobnicate`".to_string(),
        ),
        &OutputFormat::StreamJson,
        GOLDEN_CLAUDE_VERSION,
        true,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&stderr),
        String::from_utf8_lossy(&config_error_line),
        "config-error result bytes diverged from the golden error line"
    );
    assert!(stdout.is_empty(), "config errors must leave stdout empty");
}
