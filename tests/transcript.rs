use claude_print::transcript::{parse_transcript, read_transcript, AggregatedUsage};
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

fn write_jsonl(path: &Path, lines: &[String]) {
    let mut f = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(f, "{}", line).unwrap();
    }
}

fn assistant_event(id: &str, text: &str, in_tok: u64, out_tok: u64, cache_create: u64, cache_read: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "id": id,
            "content": [{"type": "text", "text": text}],
            "usage": {
                "input_tokens": in_tok,
                "output_tokens": out_tok,
                "cache_creation_input_tokens": cache_create,
                "cache_read_input_tokens": cache_read
            }
        }
    })
    .to_string()
}

fn assistant_event_no_id(usage_in: u64, usage_out: u64, text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [{"type": "text", "text": text}],
            "usage": {
                "input_tokens": usage_in,
                "output_tokens": usage_out,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }
    })
    .to_string()
}

fn result_event(session_id: &str, is_error: bool) -> String {
    serde_json::json!({
        "type": "result",
        "session_id": session_id,
        "is_error": is_error
    })
    .to_string()
}

// ── Single turn, single text block ───────────────────────────────────────────

#[test]
fn test_single_turn_single_text_block() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[assistant_event("msg-1", "hello world", 10, 5, 0, 0)],
    );
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "hello world");
    assert_eq!(r.num_turns, 1);
    assert_eq!(r.usage.input_tokens, 10);
    assert_eq!(r.usage.output_tokens, 5);
}

// ── Multi-block content: text + tool_use + thinking + text → text concatenated

#[test]
fn test_multi_block_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-2",
            "content": [
                {"type": "text", "text": "first "},
                {"type": "tool_use", "name": "bash", "id": "toolu_1", "input": {}},
                {"type": "thinking", "thinking": "reasoning here"},
                {"type": "text", "text": "second"}
            ],
            "usage": {"input_tokens": 20, "output_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
        }
    })
    .to_string();
    write_jsonl(&path, &[event]);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "first second");
    assert_eq!(r.num_turns, 1);
}

// ── Multi-turn: 3 unique usage keys → 3 turns, last turn's text returned ─────

#[test]
fn test_multi_turn_unique_keys() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("msg-a", "turn one", 10, 5, 0, 0),
            assistant_event("msg-b", "turn two", 20, 8, 0, 0),
            assistant_event("msg-c", "turn three", 30, 12, 0, 0),
        ],
    );
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.num_turns, 3);
    assert_eq!(r.text, "turn three");
    assert_eq!(r.usage.input_tokens, 60);
    assert_eq!(r.usage.output_tokens, 25);
}

// ── Streaming dedup: 5 consecutive events with identical usage → 1 turn ──────

#[test]
fn test_streaming_dedup_five_chunks() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let chunks: Vec<String> = (0..5)
        .map(|i| assistant_event("msg-stream", &format!("chunk{i}"), 10, 5, 0, 0))
        .collect();
    write_jsonl(&path, &chunks);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.num_turns, 1, "5 chunks of same message.id = 1 turn");
    assert_eq!(r.text, "chunk0chunk1chunk2chunk3chunk4");
    assert_eq!(r.usage.input_tokens, 10);
}

// ── Token aggregation: 45 unique turns → correct sum ─────────────────────────

#[test]
fn test_token_aggregation_45_turns() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let lines: Vec<String> = (0..45)
        .map(|i| assistant_event(&format!("msg-{i}"), "x", 100, 50, 10, 20))
        .collect();
    write_jsonl(&path, &lines);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.num_turns, 45);
    assert_eq!(r.usage.input_tokens, 45 * 100);
    assert_eq!(r.usage.output_tokens, 45 * 50);
    assert_eq!(r.usage.cache_creation_input_tokens, 45 * 10);
    assert_eq!(r.usage.cache_read_input_tokens, 45 * 20);
}

// ── Missing cache_creation_input_tokens → defaults to 0 ──────────────────────

#[test]
fn test_missing_cache_creation_tokens() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-3",
            "content": [{"type": "text", "text": "hello"}],
            "usage": {"input_tokens": 5, "output_tokens": 3}
        }
    })
    .to_string();
    write_jsonl(&path, &[event]);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "hello");
    assert_eq!(r.usage.cache_creation_input_tokens, 0);
}

// ── input_tokens: null → treated as 0 ────────────────────────────────────────

#[test]
fn test_null_input_tokens() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-4",
            "content": [{"type": "text", "text": "text"}],
            "usage": {"input_tokens": null, "output_tokens": 7, "cache_creation_input_tokens": null, "cache_read_input_tokens": null}
        }
    })
    .to_string();
    write_jsonl(&path, &[event]);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.usage.input_tokens, 0);
    assert_eq!(r.usage.output_tokens, 7);
    assert_eq!(r.usage.cache_creation_input_tokens, 0);
}

// ── Unknown event type → silently skipped ────────────────────────────────────

#[test]
fn test_unknown_event_type_skipped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            r#"{"type":"new-future-event","data":{"foo":42}}"#.to_string(),
            assistant_event("msg-5", "real text", 10, 5, 0, 0),
        ],
    );
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "real text");
    assert_eq!(r.num_turns, 1);
}

// ── Unknown content block type → skipped, text blocks still extracted ─────────

#[test]
fn test_unknown_content_block_skipped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-6",
            "content": [
                {"type": "image", "source": {"type": "base64", "data": "abc"}},
                {"type": "text", "text": "still here"}
            ],
            "usage": {"input_tokens": 5, "output_tokens": 3, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
        }
    })
    .to_string();
    write_jsonl(&path, &[event]);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "still here");
}

// ── Unknown usage fields → silently ignored ───────────────────────────────────

#[test]
fn test_unknown_usage_fields_ignored() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    let event = serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg-7",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 8,
                "output_tokens": 4,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "future_token_field": 999,
                "nested_future": {"a": 1}
            }
        }
    })
    .to_string();
    write_jsonl(&path, &[event]);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.usage.input_tokens, 8);
    assert_eq!(r.usage.output_tokens, 4);
}

// ── Malformed JSONL line → skipped, subsequent lines parsed ──────────────────

#[test]
fn test_malformed_line_skipped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            r#"{"type":"assistant","message":{"id":"msg-bad","content":[{"type":"text""#
                .to_string(), // truncated
            assistant_event("msg-8", "recovered", 5, 3, 0, 0),
        ],
    );
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "recovered");
    assert_eq!(r.num_turns, 1);
}

// ── Empty file → empty text, zero token counts, no panic ─────────────────────

#[test]
fn test_empty_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    std::fs::File::create(&path).unwrap();
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.text, "");
    assert_eq!(r.num_turns, 0);
    assert_eq!(r.usage, AggregatedUsage::default());
}

// ── Usage-fingerprint fallback dedup (no message.id) ─────────────────────────

#[test]
fn test_fingerprint_dedup_no_id() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    // 3 chunks with same usage but no message.id → 1 turn
    let chunks: Vec<String> = (0..3)
        .map(|i| assistant_event_no_id(10, 5, &format!("p{i}")))
        .collect();
    write_jsonl(&path, &chunks);
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.num_turns, 1);
    assert_eq!(r.text, "p0p1p2");
}

// ── Result event: session_id and is_error extracted ──────────────────────────

#[test]
fn test_result_event_fields() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.jsonl");
    write_jsonl(
        &path,
        &[
            assistant_event("msg-9", "response", 5, 3, 0, 0),
            result_event("session-xyz", false),
        ],
    );
    let r = parse_transcript(&path).unwrap();
    assert_eq!(r.session_id.as_deref(), Some("session-xyz"));
    assert!(!r.is_error);
}

// ── read_transcript: fallback to last_assistant_message ──────────────────────

#[test]
fn test_fallback_to_last_assistant_message() {
    let dir = TempDir::new().unwrap();
    let _path = dir.path().join("nonexistent.jsonl");
    // File doesn't exist; retries would time out, so use a real file with no text
    // For speed, use a transcript with no text content and provide fallback
    let path2 = dir.path().join("empty.jsonl");
    // Write a file that has no text (only a result event without assistant)
    write_jsonl(&path2, &[result_event("s1", false)]);

    let r = read_transcript(&path2, Some("fallback text")).unwrap();
    assert_eq!(r.text, "fallback text");
    assert!(r.used_fallback);
}

// ── read_transcript: error when both empty ────────────────────────────────────

#[test]
fn test_both_empty_returns_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("empty.jsonl");
    // File with result event only (no assistant text) and no fallback
    write_jsonl(&path, &[result_event("s2", false)]);
    let result = read_transcript(&path, None);
    assert!(result.is_err());
}

// ── test_streaming_dedup_40_retries: race + dedup combined ───────────────────
// Simulates Stop-before-JSONL-flush (AS-6 / MOCK_DELAY_JSONL=100):
// JSONL is written after 100ms; retry loop (40×50ms = 2s budget) catches it.
// Also verifies that streaming chunks (same message.id) are correctly deduped.

#[test]
fn test_streaming_dedup_40_retries() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("race.jsonl");

    let path_clone = path.clone();
    std::thread::spawn(move || {
        let delay_ms: u64 = std::env::var("MOCK_DELAY_JSONL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));

        // 5 streaming chunks of the same turn (same message.id)
        let mut content = String::new();
        for i in 0..5 {
            let line = serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": "msg-race-1",
                    "content": [{"type": "text", "text": format!("chunk{i}")}],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
                }
            })
            .to_string();
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(&path_clone, content).unwrap();
    });

    let r = read_transcript(&path, None).unwrap();
    assert_eq!(r.num_turns, 1, "5 streaming chunks of same message.id = 1 turn");
    assert_eq!(r.text, "chunk0chunk1chunk2chunk3chunk4");
    assert_eq!(r.usage.input_tokens, 10);
}

// ── test_transcript_race: MOCK_DELAY_JSONL=100 ───────────────────────────────
// Direct test for the race window mitigation (AS-6).

#[test]
fn test_transcript_race() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("race2.jsonl");

    let path_clone = path.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(
            &path_clone,
            format!("{}\n", assistant_event("msg-race-2", "race result", 10, 5, 0, 0)),
        )
        .unwrap();
    });

    let r = read_transcript(&path, None).unwrap();
    assert_eq!(r.text, "race result");
    assert_eq!(r.num_turns, 1);
}
