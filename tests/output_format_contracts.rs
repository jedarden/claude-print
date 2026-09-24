//! Output-format contract pin (bead claudepr-1a89e5b4).
//!
//! `docs/notes/output-format-contracts.md` is the normative definition of what
//! `--output-format` `text`, `json`, and `stream-json` write to stdout and
//! stderr: fields, framing, ordering, and error shapes. This test keeps that
//! document honest by loading `tests/fixtures/output_format_examples_v1.json`
//! and checking three layers:
//!
//! 1. **Emitter alignment** — every fixture case is replayed through
//!    `emit_success` / `emit_error` / the stream-json reader, and the captured
//!    stdout/stderr must equal the fixture's expected bytes exactly. Field
//!    sets, key order, single-line framing, and the stdout/stderr routing
//!    matrix (config errors to stderr, the after-inject switch, the
//!    stream-json success no-op) are all covered by the byte comparison.
//! 2. **Error-table alignment** — the fixture's `error_subtypes` table is
//!    asserted against `ClaudePrintError`'s `subtype()` / `exit_code()` /
//!    `message()` accessors, so the doc's exit-code table cannot drift from
//!    `src/error.rs`.
//! 3. **Doc alignment** — every fixture case marked `documented: true` whose
//!    payload is non-empty must appear verbatim in the document (trailing
//!    newline aside), so an example edited in the doc without the fixture —
//!    or vice versa — fails here. A case whose payload is empty (the
//!    stream-json success no-op) has no bytes to search for and is pinned by
//!    layer 1 alone.
//!
//! A contract change therefore updates implementation, fixture, and document
//! together in one commit — which is the point.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use claude_print::cli::OutputFormat;
use claude_print::emitter::{emit_error, emit_success, spawn_stream_json_reader_to};
use claude_print::error::ClaudePrintError;
use claude_print::transcript::{AggregatedUsage, TranscriptResult};

const FIXTURE: &str = include_str!("fixtures/output_format_examples_v1.json");
const DOC: &str = include_str!("../docs/notes/output-format-contracts.md");

// ── fixture schema ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Fixture {
    contract_version: String,
    document: String,
    pinned_by: String,
    error_subtypes: Vec<ErrorSubtype>,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct ErrorSubtype {
    variant: String,
    subtype: String,
    exit_code: i32,
    message: String,
}

#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    documented: bool,
    op: Op,
    expected_stdout: String,
    expected_stderr: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Op {
    EmitSuccess {
        format: String,
        result: ResultSpec,
        claude_version: String,
        duration_ms: u64,
    },
    EmitError {
        format: String,
        variant: String,
        message: Option<String>,
        claude_version: String,
        after_inject: bool,
    },
    StreamReplay {
        transcript_lines: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
struct ResultSpec {
    text: String,
    num_turns: usize,
    session_id: Option<String>,
    is_error: bool,
    used_fallback: bool,
    usage: UsageSpec,
}

#[derive(Debug, Deserialize)]
struct UsageSpec {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("output_format_examples_v1.json must parse")
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn format_from(s: &str) -> OutputFormat {
    match s {
        "text" => OutputFormat::Text,
        "json" => OutputFormat::Json,
        "stream-json" => OutputFormat::StreamJson,
        other => panic!("fixture names unknown output format {other:?}"),
    }
}

/// Build the `ClaudePrintError` the fixture's `variant` names. String-carrying
/// variants require the fixture's `message`; `Timeout`/`Interrupted` carry none.
fn error_from(variant: &str, message: Option<&str>) -> ClaudePrintError {
    let msg = || message.unwrap_or_else(|| panic!("variant {variant:?} needs a message"));
    match variant {
        "setup" => ClaudePrintError::Setup(msg().to_string()),
        "config" => ClaudePrintError::Config(msg().to_string()),
        "timeout" => ClaudePrintError::Timeout,
        "interrupted" => ClaudePrintError::Interrupted,
        "assistant_error" => ClaudePrintError::AssistantError(msg().to_string()),
        other => panic!("fixture names unknown error variant {other:?}"),
    }
}

/// A writer the test can poll while the reader thread runs, so replay cases
/// wait for the bytes they expect instead of sleeping past the fact.
#[derive(Clone)]
struct SharedWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn len(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    fn bytes(&self) -> Vec<u8> {
        self.buf.lock().unwrap().clone()
    }
}

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wait_for_len(writer: &SharedWriter, min_len: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while writer.len() < min_len {
        assert!(
            Instant::now() < deadline,
            "reader forwarded {} of {min_len} expected bytes within 5s — got: {:?}",
            writer.len(),
            String::from_utf8_lossy(&writer.bytes()),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ── layer 1: emitter alignment ───────────────────────────────────────────────

/// Every fixture case replays through the emitter with stdout/stderr
/// byte-compared against the fixture's expected examples. This is the layer
/// that actually pins the wire contract: field sets, key order, one-line
/// framing, LF termination, and which stream each shape lands on.
#[test]
fn fixture_cases_replay_byte_for_byte() {
    let fx = fixture();

    for case in &fx.cases {
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();

        match &case.op {
            Op::EmitSuccess {
                format,
                result,
                claude_version,
                duration_ms,
            } => {
                let spec: TranscriptResult = TranscriptResult {
                    text: result.text.clone(),
                    num_turns: result.num_turns,
                    usage: AggregatedUsage {
                        input_tokens: result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                        cache_creation_input_tokens: result.usage.cache_creation_input_tokens,
                        cache_read_input_tokens: result.usage.cache_read_input_tokens,
                    },
                    session_id: result.session_id.clone(),
                    is_error: result.is_error,
                    used_fallback: result.used_fallback,
                };
                // Success payload goes to stdout in every mode (the doc's
                // ground rule 1); emit_success has a single writer.
                emit_success(
                    &mut stdout,
                    &spec,
                    &format_from(format),
                    claude_version,
                    *duration_ms,
                )
                .unwrap();
            }
            Op::EmitError {
                format,
                variant,
                message,
                claude_version,
                after_inject,
            } => {
                emit_error(
                    &mut stdout,
                    &mut stderr,
                    &error_from(variant, message.as_deref()),
                    &format_from(format),
                    claude_version,
                    *after_inject,
                )
                .unwrap();
            }
            Op::StreamReplay { transcript_lines } => {
                // The doc's stream-json example: blank line dropped, trailing
                // CR trimmed, everything else forwarded verbatim. Lines are
                // joined with LF and the file ends LF-terminated, so the
                // CRLF record is the terminated-form case.
                let dir = tempfile::tempdir().unwrap();
                let transcript = dir.path().join("contract-replay.jsonl");
                let mut bytes = transcript_lines.join("\n").into_bytes();
                bytes.push(b'\n');
                std::fs::write(&transcript, &bytes).unwrap();

                let writer = SharedWriter::new();
                let handle = spawn_stream_json_reader_to(
                    transcript,
                    0,
                    Box::new(writer.clone()) as Box<dyn std::io::Write + Send>,
                );
                wait_for_len(&writer, case.expected_stdout.len());
                handle.signal_drain();
                drop(handle); // joins the reader thread

                stdout = writer.bytes();
            }
        }

        assert_eq!(
            String::from_utf8_lossy(&stdout),
            case.expected_stdout,
            "case {}: stdout bytes diverged from the fixture",
            case.id
        );
        assert_eq!(
            String::from_utf8_lossy(&stderr),
            case.expected_stderr,
            "case {}: stderr bytes diverged from the fixture",
            case.id
        );
    }
}

// ── layer 2: error-table alignment ───────────────────────────────────────────

/// The fixture's `error_subtypes` table is the doc's exit-code table in data
/// form; asserting it against the accessors keeps the doc, the fixture, and
/// `src/error.rs` from drifting apart independently.
#[test]
fn error_subtypes_match_error_accessors() {
    let fx = fixture();

    for row in &fx.error_subtypes {
        let e = error_from(&row.variant, Some(&row.message));
        assert_eq!(
            e.subtype(),
            row.subtype,
            "variant {}: subtype() diverged from the fixture",
            row.variant
        );
        assert_eq!(
            e.exit_code(),
            row.exit_code,
            "variant {}: exit_code() diverged from the fixture",
            row.variant
        );
        assert_eq!(
            e.message(),
            row.message,
            "variant {}: message() diverged from the fixture",
            row.variant
        );
    }
}

// ── layer 3: doc alignment ───────────────────────────────────────────────────

/// Every documented example must appear in the doc verbatim (trailing newline
/// aside). An example whose payload is empty has nothing to search for and is
/// pinned by the byte layer alone.
#[test]
fn documented_examples_appear_verbatim_in_the_doc() {
    let fx = fixture();

    for case in &fx.cases {
        if !case.documented {
            continue;
        }
        let payload = if !case.expected_stdout.is_empty() {
            &case.expected_stdout
        } else {
            &case.expected_stderr
        };
        let example = payload.strip_suffix('\n').unwrap_or(payload);
        if example.is_empty() {
            continue; // e.g. stream-json-success-emits-nothing
        }
        assert!(
            DOC.contains(example),
            "case {} is documented: true but its example does not appear verbatim in \
             docs/notes/output-format-contracts.md — update doc and fixture together",
            case.id
        );
    }
}

// ── self-integrity ───────────────────────────────────────────────────────────

/// The fixture's own header must still point at this test, this document, and
/// the contract version the doc's table row advertises — the pointers that
/// make the three-layer pin navigable.
#[test]
fn fixture_header_points_at_this_test_and_doc() {
    let fx = fixture();

    assert_eq!(
        fx.pinned_by, "tests/output_format_contracts.rs",
        "fixture pinned_by must name this test file"
    );
    assert_eq!(
        fx.document, "docs/notes/output-format-contracts.md",
        "fixture document must name the contract doc"
    );
    assert!(
        DOC.contains(&format!(
            "| **Contract version** | {} |",
            fx.contract_version
        )),
        "the doc's Contract version row must match the fixture's contract_version"
    );

    let mut ids: Vec<&str> = fx.cases.iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    let dupes = ids.windows(2).filter(|w| w[0] == w[1]).count();
    assert_eq!(dupes, 0, "fixture case ids must be unique");
}
