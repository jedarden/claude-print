use claude_print::cli::OutputFormat;
use claude_print::emitter::{emit_error, emit_success, spawn_stream_json_reader_to};
use claude_print::error::ClaudePrintError;
use claude_print::transcript::{AggregatedUsage, TranscriptResult};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn make_result(text: &str) -> TranscriptResult {
    TranscriptResult {
        text: text.to_string(),
        num_turns: 2,
        usage: AggregatedUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 5,
        },
        session_id: Some("test-session-id".to_string()),
        is_error: false,
        used_fallback: false,
    }
}

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

// ── text format ──────────────────────────────────────────────────────────────

#[test]
fn test_text_correct_string_trailing_newline() {
    let result = make_result("hello world");
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Text, "2.1.168", 0).unwrap();
    let output = buf.lock().unwrap().clone();
    assert_eq!(output, b"hello world\n");
}

#[test]
fn test_text_no_extra_whitespace() {
    let result = make_result("response");
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Text, "1.0", 0).unwrap();
    let output = buf.lock().unwrap();
    let s = std::str::from_utf8(&output).unwrap();
    assert_eq!(s.trim_end_matches('\n'), "response");
    assert!(s.ends_with('\n'));
    assert!(!s.starts_with(' '));
}

// ── json format ──────────────────────────────────────────────────────────────

#[test]
fn test_json_valid_with_required_fields() {
    let result = make_result("the answer");
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.1.168", 4200).unwrap();
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    assert_eq!(v["result"], "the answer");
    assert!(v.get("session_id").is_some());
    assert!(v.get("num_turns").is_some());
    assert!(v.get("duration_ms").is_some());
    assert!(v.get("cost_usd").is_some());
    assert!(v.get("usage").is_some());
    assert!(v.get("claude_version").is_some());
}

#[test]
fn test_json_claude_version_included() {
    let result = make_result("text");
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "2.1.168", 0).unwrap();
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["claude_version"], "2.1.168");
}

#[test]
fn test_json_usage_fields_are_integers() {
    let result = make_result("text");
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let usage = &v["usage"];
    assert!(
        usage["input_tokens"].is_u64(),
        "input_tokens must be integer"
    );
    assert!(
        usage["output_tokens"].is_u64(),
        "output_tokens must be integer"
    );
    assert!(usage["cache_creation_input_tokens"].is_u64());
    assert!(usage["cache_read_input_tokens"].is_u64());
}

// bf-416c: emit_success reads result.is_error rather than hardcoding false, as
// defense in depth. In normal operation session.rs converts an errored
// transcript into an Err before this is reached, so is_error is always false
// here — but the emitted flag must reflect the real transcript state, never lie.
#[test]
fn test_json_is_error_reflects_transcript_flag() {
    let mut result = make_result("ok");
    result.is_error = false;
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf.lock().unwrap()).unwrap();
    assert_eq!(v["is_error"], false, "success transcript → is_error false");

    let mut result = make_result("rate limited");
    result.is_error = true;
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf.lock().unwrap()).unwrap();
    assert_eq!(
        v["is_error"], true,
        "errored transcript must surface is_error=true (defense in depth)"
    );
}

// ── error result ─────────────────────────────────────────────────────────────

#[test]
fn test_error_result_is_error_true_and_subtype() {
    let err = ClaudePrintError::Timeout;
    let (out_buf, mut stdout) = capture();
    let (_, mut stderr) = capture();
    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::Json,
        "1.0",
        false,
    )
    .unwrap();
    let output = out_buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "timeout");
}

#[test]
fn test_error_exit_code_nonzero() {
    assert_ne!(ClaudePrintError::Setup("x".to_string()).exit_code(), 0);
    assert_ne!(ClaudePrintError::Config("x".to_string()).exit_code(), 0);
    assert_ne!(ClaudePrintError::Timeout.exit_code(), 0);
    assert_ne!(ClaudePrintError::Interrupted.exit_code(), 0);
    assert_ne!(
        ClaudePrintError::AssistantError("x".to_string()).exit_code(),
        0
    );
}

#[test]
fn test_error_subtypes() {
    assert_eq!(
        ClaudePrintError::Setup("x".to_string()).subtype(),
        "internal_error"
    );
    assert_eq!(
        ClaudePrintError::Config("x".to_string()).subtype(),
        "internal_error"
    );
    assert_eq!(ClaudePrintError::Timeout.subtype(), "timeout");
    assert_eq!(ClaudePrintError::Interrupted.subtype(), "interrupted");
    assert_eq!(
        ClaudePrintError::AssistantError("x".to_string()).subtype(),
        "assistant_error"
    );
}

#[test]
fn test_error_exit_codes() {
    assert_eq!(ClaudePrintError::Setup("x".to_string()).exit_code(), 2);
    assert_eq!(ClaudePrintError::Config("x".to_string()).exit_code(), 2);
    assert_eq!(ClaudePrintError::Timeout.exit_code(), 124);
    assert_eq!(ClaudePrintError::Interrupted.exit_code(), 130);
    assert_eq!(
        ClaudePrintError::AssistantError("x".to_string()).exit_code(),
        1
    );
}

#[test]
fn test_text_error_goes_to_stderr_not_stdout() {
    let err = ClaudePrintError::Setup("missing binary".to_string());
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

#[test]
fn test_json_config_error_is_structured_on_stderr() {
    let err = ClaudePrintError::Config("invalid config: malformed TOML".to_string());
    let (out_buf, mut stdout) = capture();
    let (err_buf, mut stderr) = capture();

    emit_error(
        &mut stdout,
        &mut stderr,
        &err,
        &OutputFormat::Json,
        "1.0",
        false,
    )
    .unwrap();

    assert!(out_buf.lock().unwrap().is_empty());
    let output = err_buf.lock().unwrap().clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["type"], "result");
    assert_eq!(value["is_error"], true);
    assert_eq!(value["subtype"], "internal_error");
    assert!(value["error_message"]
        .as_str()
        .unwrap()
        .contains("invalid config"));
}

// ── zero token counts ─────────────────────────────────────────────────────────

#[test]
fn test_zero_token_counts_when_fallback() {
    let result = TranscriptResult {
        text: "fallback text".to_string(),
        num_turns: 0,
        usage: AggregatedUsage::default(),
        session_id: None,
        is_error: false,
        used_fallback: true,
    };
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let usage = &v["usage"];
    assert!(usage.get("input_tokens").is_some(), "usage must be present");
    assert_eq!(usage["input_tokens"], 0);
    assert_eq!(usage["output_tokens"], 0);
    assert_eq!(usage["cache_creation_input_tokens"], 0);
    assert_eq!(usage["cache_read_input_tokens"], 0);
}

// ── EC-9: fallback ANSI stripping in the text/json emitter path ───────────────
//
// `read_transcript` is the primary sanitizer, but `emit_success` strips again
// (gated on `used_fallback`) as defense in depth. These tests exercise the
// emitter path directly by handing it a fallback result that still carries raw
// ANSI escapes.

#[test]
fn test_emit_success_strips_ansi_from_fallback_in_text_and_json() {
    let mut result = make_result("\x1b[31mred\x1b[0m green");
    result.used_fallback = true;

    // text format
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Text, "1.0", 0).unwrap();
    let text_out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(text_out, "red green\n");
    assert!(!text_out.contains('\x1b'));

    // json format
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&buf.lock().unwrap()).unwrap();
    assert_eq!(v["result"], "red green");
    assert!(!v["result"].as_str().unwrap().contains('\x1b'));
}

#[test]
fn test_emit_success_does_not_strip_normal_text() {
    // Normal (non-fallback) text with an ESC byte passes through verbatim —
    // EC-9 must not alter legitimate JSONL-sourced assistant output.
    let result = make_result("raw \x1b[31mcolor\x1b[0m text"); // used_fallback == false
    let (buf, mut writer) = capture();
    emit_success(&mut writer, &result, &OutputFormat::Text, "1.0", 0).unwrap();
    let text_out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(text_out, "raw \x1b[31mcolor\x1b[0m text\n");
}

// ── stream-json ───────────────────────────────────────────────────────────────

#[test]
fn test_stream_json_each_line_parses_as_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("transcript.jsonl");

    let lines = vec![
        r#"{"type":"assistant","message":{"id":"msg-1","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        r#"{"type":"result","is_error":false,"session_id":"abc123"}"#,
    ];
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for line in &lines {
            writeln!(f, "{}", line).unwrap();
        }
    }

    let output_buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Box::new(CaptureWriter(Arc::clone(&output_buf)));

    let handle = spawn_stream_json_reader_to(path, 0, writer);
    handle.signal_drain();
    drop(handle); // disconnect + join (INV-8); drains remaining lines first

    let output = output_buf.lock().unwrap().clone();
    let text = std::str::from_utf8(&output).unwrap();
    let output_lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(output_lines.len(), lines.len(), "should forward all lines");
    for line in &output_lines {
        let _: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|_| panic!("line is not valid JSON: {line}"));
    }
}

#[test]
fn test_stream_json_disconnect_exits_immediately() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("transcript.jsonl");
    std::fs::write(&path, b"").unwrap();

    let output_buf = Arc::new(Mutex::new(Vec::new()));
    let writer = Box::new(CaptureWriter(Arc::clone(&output_buf)));

    let handle = spawn_stream_json_reader_to(path, 0, writer);
    // No drain signal — Drop disconnects the channel, so the thread exits
    // immediately. Must not hang.
    drop(handle);
}

// bf-l69i: Test JSON serialization with very long result text
#[test]
fn test_json_long_text_serializes() {
    let long_text = "x".repeat(100_000);
    let mut result = make_result(&long_text);
    let (buf, mut writer) = capture();
    // Should not panic on unwrap at lines 68 and 105
    let result = emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0);
    assert!(result.is_ok(), "Should successfully serialize long text");
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["result"], long_text);
}

// bf-l69i: Test JSON error serialization with complex error messages
#[test]
fn test_json_error_with_special_chars() {
    let err = ClaudePrintError::Setup("Error with special chars: \n\t\r\"\\'".to_string());
    let (buf, mut writer) = capture();
    let (_, mut stderr) = capture();
    // Should not panic on unwrap at line 105
    let result = emit_error(
        &mut writer,
        &mut stderr,
        &err,
        &OutputFormat::Json,
        "1.0",
        false,
    );
    assert!(
        result.is_ok(),
        "Should successfully serialize error with special chars"
    );
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(v["error_message"].is_string());
}

// bf-l69i: Test JSON with all usage fields at maximum values
#[test]
fn test_json_max_usage_values() {
    let mut result = make_result("test");
    result.usage = AggregatedUsage {
        input_tokens: u64::MAX,
        output_tokens: u64::MAX,
        cache_creation_input_tokens: u64::MAX,
        cache_read_input_tokens: u64::MAX,
    };
    let (buf, mut writer) = capture();
    // Should not panic on unwrap at line 68
    let result = emit_success(&mut writer, &result, &OutputFormat::Json, "1.0", 0);
    assert!(result.is_ok(), "Should successfully serialize max values");
    let output = buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["usage"]["input_tokens"], u64::MAX);
}
