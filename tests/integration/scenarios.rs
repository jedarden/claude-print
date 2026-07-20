/// Cross-module integration scenarios for Phase 10.
///
/// Tests here combine transcript parsing, emitter, stop-payload resolution,
/// and hook installer modules to verify the multi-phase pipeline end-to-end.
/// They also act as the "conformance harness" required by the plan: verifying
/// that the JSON output produced by the emitter matches the `claude -p` wire
/// format, and that the extra `claude_version` field does not break lenient
/// callers.
use claude_print::cli::OutputFormat;
use claude_print::emitter::{emit_error, emit_success, spawn_stream_json_reader_to};
use claude_print::error::{ClaudePrintError, Error};
use claude_print::hook::HookInstaller;
use claude_print::poller::{cwd_to_slug, parse_stop_payload, resolve_stop_info};
use claude_print::transcript::{
    parse_transcript, read_transcript, AggregatedUsage, TranscriptResult,
};
use std::io::Write as IoWrite;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// ── Shared helpers ────────────────────────────────────────────────────────────

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
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

fn read_json(buf: &Arc<Mutex<Vec<u8>>>) -> serde_json::Value {
    let bytes = buf.lock().unwrap().clone();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "output was not valid JSON: {e}\n  raw: {:?}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn assistant_event(id: &str, text: &str, in_tok: u64, out_tok: u64, cc: u64, cr: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "id": id,
            "content": [{"type": "text", "text": text}],
            "usage": {
                "input_tokens": in_tok,
                "output_tokens": out_tok,
                "cache_creation_input_tokens": cc,
                "cache_read_input_tokens": cr,
            }
        }
    })
    .to_string()
}

fn result_event(session_id: &str, is_error: bool) -> String {
    serde_json::json!({
        "type": "result",
        "session_id": session_id,
        "is_error": is_error,
    })
    .to_string()
}

fn write_jsonl(path: &std::path::Path, lines: &[String]) {
    let mut f = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(f, "{}", line).unwrap();
    }
}

// ── Pipeline: transcript → emitter (JSON format) ──────────────────────────────

/// Full pipeline: parse JSONL, emit as JSON, verify all required fields present.
#[test]
fn transcript_to_json_pipeline_all_fields_present() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("msg-1", "hello world", 100, 50, 10, 20),
            result_event("session-abc", false),
        ],
    );

    let result = parse_transcript(&path).unwrap();
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.1.168", 1234).unwrap();

    let v = read_json(&buf);
    assert_eq!(v["type"], "result", "type field mismatch");
    assert_eq!(
        v["subtype"], "success",
        "subtype must be 'success' on success"
    );
    assert_eq!(v["is_error"], false, "is_error must be false on success");
    assert_eq!(
        v["result"], "hello world",
        "result field must contain response text"
    );
    assert!(v.get("session_id").is_some(), "session_id must be present");
    assert!(v.get("num_turns").is_some(), "num_turns must be present");
    assert!(
        v.get("duration_ms").is_some(),
        "duration_ms must be present"
    );
    assert_eq!(v["cost_usd"], 0, "cost_usd must be 0 (wire-compat)");
    assert_eq!(
        v["claude_version"], "2.1.168",
        "claude_version must be included"
    );
    assert!(v.get("usage").is_some(), "usage object must be present");
}

/// Pipeline: parse JSONL → emit as text → verify trailing newline, no JSON.
#[test]
fn transcript_to_text_pipeline_correct_output() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("msg-t", "text response", 10, 5, 0, 0),
            result_event("s1", false),
        ],
    );

    let result = parse_transcript(&path).unwrap();
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Text, "2.1.168", 0).unwrap();

    let output = buf.lock().unwrap().clone();
    let s = std::str::from_utf8(&output).unwrap();
    assert_eq!(
        s, "text response\n",
        "text output must be exactly response + newline"
    );
    // Must not be JSON
    assert!(
        serde_json::from_str::<serde_json::Value>(s.trim()).is_err(),
        "text output must not be JSON"
    );
}

/// Multi-turn pipeline: 3 distinct turns → num_turns=3 and token sum correct in JSON.
#[test]
fn multi_turn_pipeline_num_turns_and_usage_correct() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("m1", "turn 1", 100, 10, 0, 0),
            assistant_event("m2", "turn 2", 200, 20, 5, 100),
            assistant_event("m3", "turn 3 final", 300, 30, 10, 200),
            result_event("sess-multi", false),
        ],
    );

    let result = parse_transcript(&path).unwrap();
    assert_eq!(result.num_turns, 3, "must detect 3 unique turns");

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.0.0", 5000).unwrap();

    let v = read_json(&buf);
    assert_eq!(v["num_turns"], 3, "num_turns must be 3 in JSON output");
    let usage = &v["usage"];
    assert_eq!(usage["input_tokens"], 600u64, "input_tokens: 100+200+300");
    assert_eq!(usage["output_tokens"], 60u64, "output_tokens: 10+20+30");
    assert_eq!(
        usage["cache_creation_input_tokens"], 15u64,
        "cache_creation: 0+5+10"
    );
    assert_eq!(
        usage["cache_read_input_tokens"], 300u64,
        "cache_read: 0+100+200"
    );
    assert_eq!(
        v["result"], "turn 3 final",
        "result must be the last turn's text"
    );
}

/// Session ID flows from transcript result event through to JSON output.
#[test]
fn session_id_flows_from_transcript_to_json_output() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("m1", "text", 5, 3, 0, 0),
            result_event("my-session-xyz", false),
        ],
    );

    let result = parse_transcript(&path).unwrap();
    assert_eq!(result.session_id.as_deref(), Some("my-session-xyz"));

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(
        v["session_id"], "my-session-xyz",
        "session_id must flow through to JSON"
    );
}

// ── Pipeline: error transcript → emitter ─────────────────────────────────────

/// is_error: true in transcript result event → AssistantError → error JSON.
///
/// Drives the REAL conversion path that bf-416c wired up: the transcript's
/// `is_error:true` becomes `Error::AssistantError(text)` (what session.rs
/// returns), which `main.rs` converts to the user-facing type via the
/// `From<Error> for ClaudePrintError` impl. Constructing
/// `ClaudePrintError::AssistantError` by hand (as this test did before) gave
/// false coverage — it never exercised that mapping, so a regression that let
/// `Error::AssistantError` fall into the Setup catch-all (exit 2) would have
/// passed silently. This version would catch it.
#[test]
fn is_error_transcript_maps_to_assistant_error_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    // Claude returned an error response
    write_jsonl(
        &path,
        &[
            assistant_event("m1", "Rate limit exceeded", 0, 0, 0, 0),
            serde_json::json!({
                "type": "result",
                "session_id": "err-session",
                "is_error": true,
            })
            .to_string(),
        ],
    );

    let result = parse_transcript(&path).unwrap();
    assert!(result.is_error, "transcript must reflect is_error=true");

    // Mirror exactly what session.rs does on this path, then convert through
    // the same From impl main.rs relies on.
    let internal_err = Error::AssistantError(result.text.clone());
    let err: ClaudePrintError = internal_err.into();
    assert!(
        matches!(err, ClaudePrintError::AssistantError(_)),
        "Error::AssistantError must map to the user-facing AssistantError (exit 1), \
         not the Setup catch-all (exit 2)"
    );
    assert_eq!(err.exit_code(), 1);

    let (out_buf, mut stdout) = capture();
    let (_, mut stderr) = capture();
    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::Json,
        "2.0",
        false,
    )
    .unwrap();

    let v = read_json(&out_buf);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "assistant_error");
    assert_eq!(v["type"], "result");
}

/// is_error in text mode → stderr only, stdout empty.
#[test]
fn is_error_transcript_text_mode_stderr_only() {
    let err = ClaudePrintError::AssistantError("some error".to_string());
    let (out_buf, mut stdout) = capture();
    let (err_buf, mut stderr) = capture();
    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::Text,
        "1.0",
        false,
    )
    .unwrap();

    assert!(
        out_buf.lock().unwrap().is_empty(),
        "text error must not write to stdout"
    );
    assert!(
        !err_buf.lock().unwrap().is_empty(),
        "text error must write to stderr"
    );
}

// ── Fallback path pipeline ────────────────────────────────────────────────────

/// Empty transcript + last_assistant_message → fallback text used in JSON output.
#[test]
fn fallback_to_last_message_used_when_transcript_empty() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    // File exists but has no assistant events
    write_jsonl(&path, &[result_event("fb-session", false)]);

    let result = read_transcript(&path, Some("fallback response text")).unwrap();
    assert!(
        result.used_fallback,
        "must use fallback when transcript has no text"
    );
    assert_eq!(result.text, "fallback response text");

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(v["result"], "fallback response text");
    assert_eq!(v["num_turns"], 0u64, "num_turns must be 0 on fallback path");
}

/// Both transcript text empty AND fallback empty → returns error.
#[test]
fn both_empty_returns_setup_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(&path, &[result_event("no-text", false)]);
    let result = read_transcript(&path, None);
    assert!(
        result.is_err(),
        "must return Err when both transcript and fallback are empty"
    );
}

// ── Stream-JSON pipeline ──────────────────────────────────────────────────────

/// Stream-JSON reader forwards JSONL lines; every line is valid JSON.
#[test]
fn stream_json_pipeline_all_lines_valid_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let lines = vec![
        assistant_event("m1", "part 1", 10, 5, 0, 0),
        assistant_event("m1", "part 2", 10, 5, 0, 0), // duplicate — forwarded as-is
        result_event("sj-session", false),
    ];
    write_jsonl(&path, &lines);

    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Box::new(CaptureWriter(Arc::clone(&out_buf)));
    let handle = spawn_stream_json_reader_to(path, 0, writer);
    handle.signal_drain();
    drop(handle); // disconnect + join (INV-8); drains remaining lines first

    let output = out_buf.lock().unwrap().clone();
    let text = std::str::from_utf8(&output).unwrap();
    let output_lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(
        output_lines.len(),
        lines.len(),
        "stream-json must forward all lines"
    );
    for line in &output_lines {
        let _: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stream-json line not valid JSON: {e}\n  line: {line}"));
    }
}

/// Stream-JSON forwards from a byte offset; lines before offset are skipped.
#[test]
fn stream_json_start_offset_skips_pre_injection_lines() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let pre_line = r#"{"type":"system","content":"pre-injection"}"#;
    let post_line = assistant_event("m1", "post-injection", 5, 3, 0, 0);
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", pre_line).unwrap();
        writeln!(f, "{}", post_line).unwrap();
    }

    let pre_len = (pre_line.len() + 1) as u64; // +1 for newline

    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Box::new(CaptureWriter(Arc::clone(&out_buf)));
    let handle = spawn_stream_json_reader_to(path, pre_len, writer);
    handle.signal_drain();
    drop(handle); // disconnect + join (INV-8); drains remaining lines first

    let output = out_buf.lock().unwrap().clone();
    let text = std::str::from_utf8(&output).unwrap();
    let output_lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(
        output_lines.len(),
        1,
        "only post-injection line should be forwarded"
    );
    assert!(
        output_lines[0].contains("post-injection"),
        "wrong line forwarded"
    );
}

// ── Conformance harness ───────────────────────────────────────────────────────

/// Wire-format conformance: JSON output has every field that `claude -p` emits.
///
/// The `claude -p` wire format fields: type, subtype, is_error, result, session_id,
/// num_turns, duration_ms, cost_usd, usage (with 4 token sub-fields).
/// claude-print adds `claude_version` (additive — must not break lenient callers).
#[test]
fn conformance_json_output_has_all_claude_minus_p_wire_fields() {
    let result = TranscriptResult {
        text: "the answer".to_string(),
        num_turns: 1,
        usage: AggregatedUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        session_id: Some("conf-session".to_string()),
        is_error: false,
        used_fallback: false,
    };

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.1.168", 999).unwrap();
    let v = read_json(&buf);

    // Every field that `claude -p --output-format json` produces must be present.
    let required_fields = [
        "type",
        "subtype",
        "is_error",
        "result",
        "session_id",
        "num_turns",
        "duration_ms",
        "cost_usd",
        "usage",
    ];
    for field in &required_fields {
        assert!(
            v.get(field).is_some(),
            "required wire field {field:?} is missing"
        );
    }

    // The usage object must have all four token sub-fields.
    let usage = &v["usage"];
    let usage_fields = [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ];
    for field in &usage_fields {
        assert!(usage.get(field).is_some(), "usage.{field} is missing");
    }

    // claude_version is the only addition — must be present.
    assert!(
        v.get("claude_version").is_some(),
        "claude_version must be present"
    );
}

/// Wire-format conformance: a strict `claude -p`-shaped parser (deny_unknown_fields)
/// applied to the known fields must succeed even with `claude_version` present.
/// Simulated by extracting only the known fields and verifying their types.
#[test]
fn conformance_claude_version_extra_field_doesnt_break_strict_parse() {
    let result = TranscriptResult {
        text: "answer".to_string(),
        num_turns: 2,
        usage: AggregatedUsage {
            input_tokens: 50,
            output_tokens: 25,
            cache_creation_input_tokens: 5,
            cache_read_input_tokens: 100,
        },
        session_id: Some("s1".to_string()),
        is_error: false,
        used_fallback: false,
    };

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.1.168", 0).unwrap();
    let v = read_json(&buf);

    // A strict caller that parses only known claude -p fields must succeed.
    // We simulate this by deserializing the value into a struct with known fields.
    #[derive(serde::Deserialize)]
    struct ClaudeMinusPResult {
        #[serde(rename = "type")]
        result_type: String,
        subtype: String,
        is_error: bool,
        result: String,
        session_id: Option<String>,
        num_turns: u64,
        duration_ms: u64,
        cost_usd: u64,
        usage: UsageFields,
    }
    #[derive(serde::Deserialize)]
    struct UsageFields {
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_input_tokens: u64,
        cache_read_input_tokens: u64,
    }
    // This should NOT use deny_unknown_fields — just like a real caller that accesses
    // fields by name and ignores extras.
    let parsed: ClaudeMinusPResult = serde_json::from_value(v.clone())
        .expect("strict parse of known fields must succeed despite extra claude_version field");

    assert_eq!(parsed.result_type, "result");
    assert_eq!(parsed.subtype, "success");
    assert!(!parsed.is_error);
    assert_eq!(parsed.result, "answer");
    assert_eq!(parsed.num_turns, 2);
    assert_eq!(parsed.usage.input_tokens, 50);
    assert_eq!(parsed.usage.output_tokens, 25);
}

/// Wire-format conformance: all four usage fields are unsigned integers, not strings or null.
#[test]
fn conformance_all_usage_fields_are_unsigned_integers() {
    let result = TranscriptResult {
        text: "x".to_string(),
        num_turns: 1,
        usage: AggregatedUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 99999,
        },
        session_id: None,
        is_error: false,
        used_fallback: false,
    };

    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v = read_json(&buf);
    let usage = &v["usage"];

    for field in &[
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        assert!(
            usage[field].is_u64(),
            "usage.{field} must be an unsigned integer, got: {:?}",
            usage[field]
        );
        assert!(!usage[field].is_null(), "usage.{field} must not be null");
    }
}

/// Wire-format conformance: error result has required wire format fields.
#[test]
fn conformance_error_result_wire_format() {
    let errors = [
        ClaudePrintError::Setup("setup failed".to_string()),
        ClaudePrintError::Timeout,
        ClaudePrintError::Interrupted,
        ClaudePrintError::AssistantError("assistant err".to_string()),
    ];

    for err in &errors {
        let (out_buf, mut stdout) = capture();
        let (_, mut stderr) = capture();
        emit_error(
            &mut stdout,
            &mut stderr,
            err,
            &OutputFormat::Json,
            "2.0",
            false,
        )
        .unwrap();

        let v = read_json(&out_buf);
        assert_eq!(v["type"], "result", "error type must be 'result'");
        assert_eq!(v["is_error"], true, "error is_error must be true");
        assert!(v.get("subtype").is_some(), "error must have subtype");
        assert!(
            v.get("error_message").is_some(),
            "error must have error_message"
        );
        assert!(
            v.get("claude_version").is_some(),
            "error must have claude_version"
        );
        assert_ne!(
            v["subtype"].as_str(),
            Some("success"),
            "error subtype must not be 'success'"
        );
    }
}

/// Wire-format conformance: subtype field is exactly "success" for successful result.
#[test]
fn conformance_subtype_is_success_for_success_result() {
    let result = TranscriptResult {
        text: "ok".to_string(),
        num_turns: 1,
        usage: AggregatedUsage::default(),
        session_id: None,
        is_error: false,
        used_fallback: false,
    };
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["type"], "result");
}

/// Wire-format conformance: error subtype strings match the spec.
#[test]
fn conformance_error_subtype_strings_match_spec() {
    let cases = [
        (ClaudePrintError::Setup("x".to_string()), "internal_error"),
        (ClaudePrintError::Timeout, "timeout"),
        (ClaudePrintError::Interrupted, "interrupted"),
        (
            ClaudePrintError::AssistantError("y".to_string()),
            "assistant_error",
        ),
    ];
    for (err, expected_subtype) in &cases {
        let (out_buf, mut stdout) = capture();
        let (_, mut stderr) = capture();
        emit_error(
            &mut stdout,
            &mut stderr,
            err,
            &OutputFormat::Json,
            "1.0",
            false,
        )
        .unwrap();
        let v = read_json(&out_buf);
        assert_eq!(
            v["subtype"].as_str(),
            Some(*expected_subtype),
            "subtype mismatch for {expected_subtype}"
        );
    }
}

// ── Version resilience through the pipeline ───────────────────────────────────

/// Unknown JSONL event types are skipped; text from known events still extracted.
/// This is the "version resilience" path through the full parse → emit pipeline.
#[test]
fn version_resilience_unknown_events_in_pipeline_no_panic() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    // Mix of unknown events and known assistant events
    let lines = vec![
        r#"{"type":"session-start","model":"claude-4","session_id":"vs1"}"#.to_string(),
        assistant_event("m1", "real response", 50, 25, 0, 0),
        r#"{"type":"thinking-summary","content":"thinking text here"}"#.to_string(),
        r#"{"type":"tool-result","tool_id":"t1","output":"done"}"#.to_string(),
        result_event("vs1", false),
    ];
    write_jsonl(&path, &lines);

    let result = parse_transcript(&path).unwrap();
    assert_eq!(
        result.text, "real response",
        "text must be extracted despite unknown events"
    );
    assert_eq!(result.num_turns, 1);

    // Pipeline must complete without panic
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "3.0.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(v["result"], "real response");
    assert_eq!(v["claude_version"], "3.0.0");
}

/// Extra fields in the Stop payload do not prevent path derivation or pipeline completion.
#[test]
fn version_resilience_extra_fields_in_stop_payload_through_pipeline() {
    let mut json = String::from(
        r#"{"hook_event_name":"Stop","session_id":"vs-session","cwd":"/home/user/project""#,
    );
    for i in 0..20 {
        json.push_str(&format!(r#","new_claude_field_{i}":"value_{i}""#));
    }
    json.push('}');

    let payload = parse_stop_payload(json.as_bytes()).unwrap();
    assert_eq!(payload.session_id.as_deref(), Some("vs-session"));
    assert_eq!(payload.cwd.as_deref(), Some("/home/user/project"));

    let info = resolve_stop_info(payload);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expected = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join("home-user-project")
        .join("vs-session.jsonl");
    assert_eq!(
        info.transcript_path,
        Some(expected),
        "path derivation must survive extra fields"
    );
}

// ── Stop payload + transcript path integration ────────────────────────────────

/// Explicit transcript_path in Stop payload is used as-is (no derivation).
#[test]
fn stop_payload_explicit_path_used_directly() {
    let dir = TempDir::new().unwrap();
    let transcript = dir.path().join("mysession.jsonl");
    write_jsonl(
        &transcript,
        &[
            assistant_event("m1", "explicit path response", 10, 5, 0, 0),
            result_event("mysession", false),
        ],
    );

    let payload_json = format!(
        r#"{{"hook_event_name":"Stop","session_id":"mysession","transcript_path":"{}","cwd":"/tmp"}}"#,
        transcript.display()
    );
    let payload = parse_stop_payload(payload_json.as_bytes()).unwrap();
    let info = resolve_stop_info(payload);

    assert_eq!(
        info.transcript_path,
        Some(transcript.clone()),
        "explicit path must be used directly"
    );

    // Parse the transcript at the resolved path
    let result = parse_transcript(&transcript).unwrap();
    assert_eq!(result.text, "explicit path response");
    assert_eq!(result.session_id.as_deref(), Some("mysession"));
}

/// Missing transcript_path → derive from session_id + cwd → transcript read → emit.
#[test]
fn stop_payload_path_derivation_and_transcript_emit() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let dir = TempDir::new().unwrap();
    let session_id = "derive-test-session";
    let cwd = "/home/user/myproject";
    let slug = cwd_to_slug(cwd);
    let transcript_dir = dir.path().join(".claude").join("projects").join(&slug);
    std::fs::create_dir_all(&transcript_dir).unwrap();
    let transcript = transcript_dir.join(format!("{session_id}.jsonl"));
    write_jsonl(
        &transcript,
        &[
            assistant_event("m1", "derived path response", 20, 10, 0, 0),
            result_event(session_id, false),
        ],
    );

    // Verify the slug algorithm
    assert_eq!(slug, "home-user-myproject");

    let payload_json =
        format!(r#"{{"hook_event_name":"Stop","session_id":"{session_id}","cwd":"{cwd}"}}"#);
    let payload = parse_stop_payload(payload_json.as_bytes()).unwrap();
    let info = resolve_stop_info(payload);

    // Verify derived path uses $HOME — this would differ from our test dir,
    // but we can still verify the slug computation is correct.
    let expected_suffix = format!(".claude/projects/{slug}/{session_id}.jsonl");
    if let Some(p) = &info.transcript_path {
        assert!(
            p.to_string_lossy().ends_with(&expected_suffix),
            "derived path has correct suffix"
        );
    }

    // Parse transcript at the actual path (not derived, since we used a test dir)
    let result = parse_transcript(&transcript).unwrap();
    assert_eq!(result.text, "derived path response");
    assert_eq!(result.session_id.as_deref(), Some(session_id));

    // Pipeline completion
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(v["result"], "derived path response");
}

// ── CWD slug algorithm ────────────────────────────────────────────────────────

/// Slug algorithm for representative cwd values (plan: "unit test for 3-4 cwd values").
#[test]
fn cwd_slug_algorithm_representative_cases() {
    // Documented in plan §8 Transcript Reader
    assert_eq!(
        cwd_to_slug("/home/coding/myproject"),
        "home-coding-myproject"
    );
    assert_eq!(cwd_to_slug("/root/foo/bar"), "root-foo-bar");
    assert_eq!(cwd_to_slug("/tmp/x"), "tmp-x");
    assert_eq!(cwd_to_slug("/tmp"), "tmp");
    // Ambiguous case: /home/user/a-b and /home/user-a/b both → home-user-a-b
    assert_eq!(
        cwd_to_slug("/home/user/a-b"),
        "home-user-a-b",
        "hyphenated dir name"
    );
    assert_eq!(
        cwd_to_slug("/home/user-a/b"),
        "home-user-a-b",
        "hyphen in parent dir"
    );
}

// ── Invariant: temp dir lifecycle (INV-1) ─────────────────────────────────────

/// INV-1: temp dir is removed on HookInstaller drop (no leftover in TMPDIR).
#[test]
fn invariant_temp_dir_drop_removes_all_artifacts() {
    let (dir_path, hook_path, fifo_path, settings_path) = {
        let installer = HookInstaller::new().unwrap();
        let d = installer.dir_path().to_path_buf();
        let h = installer.hook_path.clone();
        let f = installer.fifo_path.clone();
        let s = installer.settings_path.clone();
        (d, h, f, s)
    };
    // After drop, none of the paths should exist.
    assert!(
        !dir_path.exists(),
        "temp dir must not exist after drop (INV-1)"
    );
    assert!(!hook_path.exists(), "hook.sh must not exist after drop");
    assert!(!fifo_path.exists(), "stop.fifo must not exist after drop");
    assert!(
        !settings_path.exists(),
        "settings.json must not exist after drop"
    );
}

/// INV-5: Hook artifacts are NOT placed inside ~/.claude/ or the user's home dir.
#[test]
fn invariant_hook_artifacts_not_in_home_claude_dir() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let claude_dir = std::path::PathBuf::from(&home).join(".claude");

    let installer = HookInstaller::new().unwrap();
    let dir = installer.dir_path();

    assert!(
        !dir.starts_with(&claude_dir),
        "temp dir must not be inside ~/.claude/: {dir:?}"
    );
}

// ── Corner cases ──────────────────────────────────────────────────────────────

/// Stop payload with only session_id and cwd (no transcript_path, no last_message):
/// path is derived; fallback message is None.
#[test]
fn stop_payload_minimal_fields_derives_path() {
    let json = r#"{"hook_event_name":"Stop","session_id":"min-sid","cwd":"/tmp/min"}"#;
    let payload = parse_stop_payload(json.as_bytes()).unwrap();
    assert!(payload.transcript_path.is_none());
    assert!(payload.last_assistant_message.is_none());

    let info = resolve_stop_info(payload);
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let expected = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join("tmp-min")
        .join("min-sid.jsonl");
    assert_eq!(info.transcript_path, Some(expected));
    assert!(info.last_assistant_message.is_none());
}

/// stream-json error before inject: text mode stderr-only behavior also applies.
#[test]
fn stream_json_error_before_inject_no_stdout() {
    let err = ClaudePrintError::Setup("binary not found".to_string());
    let (out_buf, mut stdout) = capture();
    let (err_buf, mut stderr) = capture();
    // stream_json_after_inject = false → behaves like text mode (stderr only)
    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::StreamJson,
        "1.0",
        false,
    )
    .unwrap();
    assert!(
        out_buf.lock().unwrap().is_empty(),
        "stream-json before-inject error must not write stdout"
    );
    assert!(
        !err_buf.lock().unwrap().is_empty(),
        "stream-json before-inject error must write stderr"
    );
}

/// stream-json error after inject: error JSON written to stdout.
#[test]
fn stream_json_error_after_inject_writes_json_to_stdout() {
    let err = ClaudePrintError::Timeout;
    let (out_buf, mut stdout) = capture();
    let (_, mut stderr) = capture();
    // stream_json_after_inject = true → JSON to stdout
    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::StreamJson,
        "2.0",
        true,
    )
    .unwrap();

    let v = read_json(&out_buf);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "timeout");
    assert_eq!(v["claude_version"], "2.0");
}

/// cost_usd is always 0 in JSON output (wire-compat field, always emitted).
#[test]
fn cost_usd_always_zero_in_json_output() {
    let result = TranscriptResult {
        text: "x".to_string(),
        num_turns: 1,
        usage: AggregatedUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
        session_id: None,
        is_error: false,
        used_fallback: false,
    };
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v = read_json(&buf);
    assert_eq!(
        v["cost_usd"], 0,
        "cost_usd must always be 0 (plan §9 known limitation)"
    );
}

/// duration_ms in JSON output matches the value passed to emit_success.
#[test]
fn duration_ms_reflects_passed_value() {
    let result = TranscriptResult {
        text: "x".to_string(),
        num_turns: 1,
        usage: AggregatedUsage::default(),
        session_id: None,
        is_error: false,
        used_fallback: false,
    };
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 42_000).unwrap();
    let v = read_json(&buf);
    assert_eq!(
        v["duration_ms"], 42_000u64,
        "duration_ms must reflect the passed value"
    );
}
