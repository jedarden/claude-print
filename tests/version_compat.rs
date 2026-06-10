/// Version-resilience test suite (Phase 10).
///
/// Verifies that `claude-print` survives Claude Code schema changes without
/// rebuilding.  All tests are credential-free and run in CI on every push.
use claude_print::poller::parse_stop_payload;
use claude_print::startup::StartupSeq;
use claude_print::transcript::parse_transcript;
use std::io::Write as IoWrite;
use std::path::Path;
use tempfile::TempDir;

// ── Stop payload with 50 unknown extra fields ─────────────────────────────────

#[test]
fn stop_payload_50_unknown_fields_parsed_without_error() {
    let mut json =
        String::from(r#"{"hook_event_name":"Stop","session_id":"sid1","cwd":"/tmp/x""#);
    for i in 0..50 {
        json.push_str(&format!(r#","future_field_{i}":"value_{i}""#));
    }
    json.push('}');
    let p = parse_stop_payload(json.as_bytes()).expect("must parse with 50 unknown fields");
    assert_eq!(p.session_id.as_deref(), Some("sid1"), "session_id must survive unknown fields");
    assert_eq!(p.cwd.as_deref(), Some("/tmp/x"), "cwd must survive unknown fields");
}

// ── Usage object with 20 new numeric fields ───────────────────────────────────

#[test]
fn usage_20_new_numeric_fields_ignored_known_fields_correct() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");

    let mut usage = serde_json::json!({
        "input_tokens": 100,
        "output_tokens": 50,
        "cache_creation_input_tokens": 10,
        "cache_read_input_tokens": 20,
    });
    let obj = usage.as_object_mut().unwrap();
    for i in 0..20u64 {
        obj.insert(
            format!("future_metric_{i}"),
            serde_json::Value::Number(serde_json::Number::from(i)),
        );
    }
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-new-usage",
            "content": [{"type": "text", "text": "ok"}],
            "usage": usage,
        }
    });
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "{}", event).unwrap();
    drop(file);

    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.usage.input_tokens, 100, "input_tokens wrong");
    assert_eq!(r.usage.output_tokens, 50, "output_tokens wrong");
    assert_eq!(r.usage.cache_creation_input_tokens, 10, "cache_create wrong");
    assert_eq!(r.usage.cache_read_input_tokens, 20, "cache_read wrong");
    assert_eq!(r.num_turns, 1);
}

// ── Content block with new type and required fields → Unknown via #[serde(other)]

#[test]
fn content_block_new_type_with_required_field_treated_as_unknown() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-future-block",
            "content": [
                {
                    "type": "future_rich_media",
                    "required_in_new_version": "some-value",
                    "extra_metadata": {"version": 3}
                },
                {"type": "text", "text": "extracted text"}
            ],
            "usage": {
                "input_tokens": 5, "output_tokens": 3,
                "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0
            }
        }
    });
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "{}", event).unwrap();
    drop(file);

    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "extracted text", "text after unknown block must be extracted");
    assert_eq!(r.num_turns, 1);
}

// ── JSONL with events in a new order ─────────────────────────────────────────

#[test]
fn jsonl_events_in_new_order_parse_succeeds() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    // A hypothetical "summary" event appears before user/assistant in a future version.
    let lines = [
        r#"{"type":"summary","content":"A new summary event type","model":"claude-5"}"#,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"hello"}]}}"#,
        r#"{"type":"assistant","message":{"id":"msg-ord","content":[{"type":"text","text":"world"}],"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        r#"{"type":"result","is_error":false,"session_id":"ord-session"}"#,
    ];
    let mut file = std::fs::File::create(&path).unwrap();
    for line in &lines {
        writeln!(file, "{}", line).unwrap();
    }
    drop(file);

    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "world", "text must be extracted despite new event order");
    assert_eq!(r.num_turns, 1);
    assert_eq!(r.session_id.as_deref(), Some("ord-session"));
}

// ── Startup heuristic stability: 20 trust dialog phrasings must all trigger ───

#[test]
fn startup_20_trust_dialog_phrasings_all_trigger() {
    let phrasings: &[&str] = &[
        "Do you trust and Allow access to this folder?",
        "Grant permission to proceed with this folder",
        "Please trust and continue to allow",
        "Allow and continue access to this folder",
        "Do you want to proceed and trust this folder?",
        "Permission required: trust to continue",
        "Trust this folder and proceed with Allow",
        "continue and allow this folder permission",
        "Grant trust, proceed, and allow folder access",
        "Please trust, allow, and continue in this folder",
        "Permission to proceed: trust and allow folder",
        "Trust dialog: allow and continue with folder",
        "You must trust and continue to allow folder access",
        "Do you Allow and trust this folder to proceed?",
        "Before continuing, trust and allow this folder",
        "Allow permission to proceed and trust folder",
        "This action requires trust and proceed to continue",
        "To allow folder access, trust and proceed",
        "Grant access: trust, allow, and proceed with folder",
        "Confirm permission: trust to allow and continue",
    ];
    for &phrasing in phrasings {
        assert!(
            StartupSeq::scan_line(phrasing.as_bytes()),
            "expected trust dialog trigger for: {phrasing:?}"
        );
    }
}

// ── Startup heuristic stability: 10 non-dialog lines must not trigger ─────────

#[test]
fn startup_10_non_dialog_lines_do_not_trigger() {
    let non_dialogs: &[&str] = &[
        "Initializing Claude Code v2.1.168...",
        "Loading configuration...",
        "Reading context from files",
        "Connecting to API endpoint",
        "claude-print started",
        "Processing your request",
        "",
        "   ",
        "\x1b[31mError\x1b[0m: Something went wrong",
        "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━",
    ];
    for &line in non_dialogs {
        assert!(
            !StartupSeq::scan_line(line.as_bytes()),
            "expected NO trigger for non-dialog line: {line:?}"
        );
    }
}

// ── Token count regression: fixture transcript_v2.1.168.jsonl ─────────────────

#[test]
fn token_regression_fixture_v2_1_168() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/transcript_v2.1.168.jsonl");
    let r = parse_transcript(&path).expect("parse fixture failed");
    // Turn 1: msg-001 — in=6178, out=295, cache_create=825, cache_read=26442
    // Turn 2: msg-002 × 3 streaming chunks — in=100, out=50, cache_create=0, cache_read=5000
    assert_eq!(r.num_turns, 2, "fixture has 2 unique assistant turns");
    assert_eq!(r.usage.input_tokens, 6278, "input_tokens mismatch (6178 + 100)");
    assert_eq!(r.usage.output_tokens, 345, "output_tokens mismatch (295 + 50)");
    assert_eq!(r.usage.cache_creation_input_tokens, 825, "cache_creation mismatch (825 + 0)");
    assert_eq!(r.usage.cache_read_input_tokens, 31442, "cache_read mismatch (26442 + 5000)");
    // Last turn's text is the concatenation of the 3 streaming chunks
    assert_eq!(r.text, "chunk1 chunk2 chunk3", "last turn text mismatch");
}

// ── Fixture also contains unknown usage fields → ignored ──────────────────────

#[test]
fn fixture_unknown_usage_fields_ignored() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/transcript_v2.1.168.jsonl");
    // The fixture contains `server_tool_use`, `service_tier`, `cache_creation`,
    // `inference_geo`, `speed` in the usage object — all should be silently ignored.
    let r = parse_transcript(&path).expect("parse fixture must succeed");
    assert!(r.num_turns > 0, "must parse at least one turn despite unknown usage fields");
}
