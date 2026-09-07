//! PTY/TUI transcript shape (claudepr-26e7a0b6).
//!
//! A PTY-driven TUI session's transcript differs from a print-mode one in two
//! ways that both broke `claude-print`, and neither is covered by the
//! print-mode fixtures in `version_compat.rs`:
//!   1. it carries no `type: "result"` event — that is a print/SDK construct;
//!   2. the session id is spelled `sessionId`, on every ordinary record.
//!
//! Together these meant `session_id` came back null for every real PTY run,
//! even once the transcript itself was being written again.

use claude_print::transcript::parse_transcript;
use std::io::Write as IoWrite;
use std::path::Path;
use tempfile::TempDir;

/// Build a TUI-shaped transcript: `sessionId` on each record, no result event.
fn write_tui_transcript(dir: &Path, session_id: &str) -> std::path::PathBuf {
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut f = std::fs::File::create(&path).expect("create fixture");
    for (id, text) in [("msg_1", "thinking"), ("msg_2", "42")] {
        writeln!(
            f,
            r#"{{"type":"assistant","sessionId":"{session_id}","userType":"external","message":{{"id":"{id}","content":[{{"type":"text","text":"{text}"}}],"usage":{{"input_tokens":3,"output_tokens":5}}}}}}"#
        )
        .expect("write fixture line");
    }
    path
}

#[test]
fn tui_transcript_yields_session_id_without_result_event() {
    let dir = TempDir::new().expect("tempdir");
    let sid = "86df36c7-b5dd-438f-8275-ed23749cea18";
    let path = write_tui_transcript(dir.path(), sid);

    let r = parse_transcript(&path).expect("TUI transcript should parse");

    assert_eq!(
        r.session_id.as_deref(),
        Some(sid),
        "session_id must come from ordinary records when no result event exists"
    );
    assert!(
        r.num_turns > 0,
        "TUI transcript must report real turns, not the zeroed fallback"
    );
    assert!(!r.used_fallback, "a parsed transcript is not the fallback");
}

#[test]
fn result_event_session_id_accepts_camel_case() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("with-result.jsonl");
    let mut f = std::fs::File::create(&path).expect("create fixture");
    writeln!(
        f,
        r#"{{"type":"assistant","message":{{"id":"m1","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":1,"output_tokens":1}}}}}}"#
    )
    .expect("write");
    writeln!(
        f,
        r#"{{"type":"result","sessionId":"sess-camel","is_error":false}}"#
    )
    .expect("write");
    drop(f);

    let r = parse_transcript(&path).expect("parse");
    assert_eq!(
        r.session_id.as_deref(),
        Some("sess-camel"),
        "result events spell it sessionId, not session_id"
    );
}
