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
    assert!(usage["input_tokens"].is_u64(), "input_tokens must be integer");
    assert!(usage["output_tokens"].is_u64(), "output_tokens must be integer");
    assert!(usage["cache_creation_input_tokens"].is_u64());
    assert!(usage["cache_read_input_tokens"].is_u64());
}

// ── error result ─────────────────────────────────────────────────────────────

#[test]
fn test_error_result_is_error_true_and_subtype() {
    let err = ClaudePrintError::Timeout;
    let (out_buf, mut stdout) = capture();
    let (_, mut stderr) = capture();
    emit_error(&mut stdout, &mut stderr, &err, &OutputFormat::Json, "1.0", false).unwrap();
    let output = out_buf.lock().unwrap().clone();
    let v: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "timeout");
}

#[test]
fn test_error_exit_code_nonzero() {
    assert_ne!(ClaudePrintError::Setup("x".to_string()).exit_code(), 0);
    assert_ne!(ClaudePrintError::Timeout.exit_code(), 0);
    assert_ne!(ClaudePrintError::Interrupted.exit_code(), 0);
    assert_ne!(ClaudePrintError::AssistantError("x".to_string()).exit_code(), 0);
}

#[test]
fn test_error_subtypes() {
    assert_eq!(ClaudePrintError::Setup("x".to_string()).subtype(), "internal_error");
    assert_eq!(ClaudePrintError::Timeout.subtype(), "timeout");
    assert_eq!(ClaudePrintError::Interrupted.subtype(), "interrupted");
    assert_eq!(ClaudePrintError::AssistantError("x".to_string()).subtype(), "assistant_error");
}

#[test]
fn test_error_exit_codes() {
    assert_eq!(ClaudePrintError::Setup("x".to_string()).exit_code(), 2);
    assert_eq!(ClaudePrintError::Timeout.exit_code(), 124);
    assert_eq!(ClaudePrintError::Interrupted.exit_code(), 130);
    assert_eq!(ClaudePrintError::AssistantError("x".to_string()).exit_code(), 1);
}

#[test]
fn test_text_error_goes_to_stderr_not_stdout() {
    let err = ClaudePrintError::Setup("missing binary".to_string());
    let (out_buf, mut stdout) = capture();
    let (err_buf, mut stderr) = capture();
    emit_error(&mut stdout, &mut stderr, &err, &OutputFormat::Text, "1.0", false).unwrap();
    assert!(out_buf.lock().unwrap().is_empty(), "text error must not write to stdout");
    assert!(!err_buf.lock().unwrap().is_empty(), "text error must write to stderr");
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
    handle.drain_tx.send(()).unwrap();
    handle.join_handle.join().unwrap();

    let output = output_buf.lock().unwrap().clone();
    let text = std::str::from_utf8(&output).unwrap();
    let output_lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();

    assert_eq!(output_lines.len(), lines.len(), "should forward all lines");
    for line in &output_lines {
        let _: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|_| panic!("line is not valid JSON: {line}"));
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
    // Drop drain_tx without sending — thread should exit immediately
    drop(handle.drain_tx);
    handle.join_handle.join().unwrap(); // must not hang
}
