/// Version-resilience test suite (Phase 10).
///
/// Verifies that `claude-print` survives Claude Code schema changes without
/// rebuilding.  All tests are credential-free and run in CI on every push.
use claude_print::poller::parse_stop_payload;
use claude_print::startup::{StartupAction, StartupPhase, StartupSeq};
use claude_print::transcript::parse_transcript;
use std::io::Write as IoWrite;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;

// ── Claude version format tracking ───────────────────────────────────────────

/// CI artifact: record the current claude binary version for regression
/// tracking.  If the version changes between CI runs, the operator is alerted
/// via a diff in the `last-claude-version.txt` artifact.
///
/// This test is skipped (pass) when `claude` is not on PATH — non-blocking
/// for developer machines that have the binary at a non-standard location.
#[test]
fn test_claude_version_recorded() {
    let output = match std::process::Command::new("claude")
        .arg("--version")
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            // claude not on PATH — skip test rather than fail.
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    let first_line = combined.lines().next().unwrap_or("").trim();
    // The version string must contain "Claude Code" (observed format: "2.1.168 (Claude Code)")
    assert!(
        first_line.contains("Claude Code") || first_line.contains("claude"),
        "unexpected claude --version format: {first_line:?}"
    );
    // Write to CI artifact for diff-based regression tracking.  Failure is
    // non-fatal (e.g., read-only filesystem) — the assertion above is the real gate.
    let artifact_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let _ = std::fs::create_dir_all(&artifact_dir);
    let _ = std::fs::write(
        artifact_dir.join("last-claude-version.txt"),
        first_line.as_bytes(),
    );
}

// ── Stop payload with 50 unknown extra fields ─────────────────────────────────

#[test]
fn stop_payload_50_unknown_fields_parsed_without_error() {
    let mut json = String::from(r#"{"hook_event_name":"Stop","session_id":"sid1","cwd":"/tmp/x""#);
    for i in 0..50 {
        json.push_str(&format!(r#","future_field_{i}":"value_{i}""#));
    }
    json.push('}');
    let p = parse_stop_payload(json.as_bytes()).expect("must parse with 50 unknown fields");
    assert_eq!(
        p.session_id.as_deref(),
        Some("sid1"),
        "session_id must survive unknown fields"
    );
    assert_eq!(
        p.cwd.as_deref(),
        Some("/tmp/x"),
        "cwd must survive unknown fields"
    );
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
    assert_eq!(
        r.usage.cache_creation_input_tokens, 10,
        "cache_create wrong"
    );
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
    assert_eq!(
        r.text, "extracted text",
        "text after unknown block must be extracted"
    );
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
    assert_eq!(
        r.text, "world",
        "text must be extracted despite new event order"
    );
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

// ── Startup regression: claude 2.1.263 caret-on-refuse layout ────────────────
//
// The 2.1.263 render highlights "No, exit" by default — screen-scraped in
// tests/fixtures/startup_trust_dialog_v2.1.263.txt. Confirming whatever is
// highlighted kills the session in every untrusted cwd (claudepr-fe3d3160),
// so this exact layout must stay pinned: the dismissal moves the caret onto
// the trusting entry before pressing Enter.

/// The screen-scraped claude 2.1.263 caret-on-refuse startup render.
fn fixture_v2_1_263_capture() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/startup_trust_dialog_v2.1.263.txt");
    std::fs::read(&path).expect("fixture startup_trust_dialog_v2.1.263.txt must exist")
}

/// The fixture reproduces the regressed layout: the dialog is detected and the
/// dismissal plan is one Down arrow onto "Yes, I trust this folder", then Enter.
#[test]
fn startup_fixture_v2_1_263_caret_on_refuse_moves_down_then_enter() {
    let capture = fixture_v2_1_263_capture();
    assert!(
        StartupSeq::dialog_present(&capture),
        "the screen-scraped render must detect the trust dialog"
    );
    assert_eq!(
        StartupSeq::plan_keys(&capture).as_deref(),
        Some(b"\x1b[B\r".as_slice()),
        "caret on 'No, exit' must move down to 'Yes, I trust this folder' before Enter"
    );
}

/// Full startup sequence over the fixture: keys are held while the TUI paints,
/// fire as Down+Enter once it has been quiet, then the prompt is injected as a
/// bracketed paste.
#[test]
fn startup_fixture_v2_1_263_sequence_dismisses_then_injects_prompt() {
    let capture = fixture_v2_1_263_capture();
    let mut seq = StartupSeq::with_idle_gap(b"What is 2+2?".to_vec(), 100);

    // Render burst: the dismissal is identified but held — keys written while
    // the TUI is still painting are silently dropped.
    assert!(
        matches!(seq.feed(&capture), StartupAction::None),
        "keys must be held at the parseable instant"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);

    // Quiet window elapses (DISMISS_SETTLE_MS = 400 ms): move down, then Enter.
    std::thread::sleep(Duration::from_millis(450));
    match seq.poll_timers() {
        StartupAction::Write(keys) => assert_eq!(
            keys, b"\x1b[B\r",
            "dismissal must select the trusting entry, not confirm the highlighted default"
        ),
        other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
    }
    assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

    // Post-dismiss idle gap (100 ms here): the prompt goes out as a bracketed
    // paste, verbatim, terminated by CR.
    std::thread::sleep(Duration::from_millis(150));
    match seq.poll_timers() {
        StartupAction::Write(payload) => {
            let prompt = b"What is 2+2?";
            assert!(
                payload.starts_with(b"\x1b[200~")
                    && payload.ends_with(b"\x1b[201~\r")
                    && payload.windows(prompt.len()).any(|w| w == prompt),
                "expected the bracketed-paste prompt payload, got {payload:?}"
            );
        }
        other => panic!("expected prompt injection after the idle gap, got {other:?}"),
    }
    assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
}

/// Regression guard: a blind Enter — a confirming CR with no caret movement
/// ahead of it — must fail this suite. On the pinned 2.1.263 layout the caret
/// renders on "No, exit" (❯ on the first entry), so Enter without a preceding
/// Down arrow confirms the refusing default and kills the session in every
/// untrusted cwd (claudepr-fe3d3160). Both dismissal shapes are guarded: the
/// pure planner must not plan a bare confirm, and the live state machine must
/// never emit a bare CR as its first write — which is what the no-dialog idle
/// fallback would produce if this fixture ever stopped detecting as a dialog
/// (the capture is well past the 200-byte idle threshold).
#[test]
fn startup_fixture_v2_1_263_blind_enter_without_caret_move_fails() {
    let capture = fixture_v2_1_263_capture();

    // Planner shape: the dismissal must lead with an arrow key and confirm
    // only after it; a bare CR is the blind Enter this fixture pins against.
    let planned = StartupSeq::plan_keys(&capture)
        .expect("the trusting entry must be positively identified for this layout");
    assert_ne!(
        planned, b"\r",
        "blind Enter: no caret movement ahead of the confirming CR — against \
         this layout that selects the highlighted 'No, exit'"
    );
    assert!(
        planned.starts_with(b"\x1b[") && planned.ends_with(b"\r"),
        "dismissal must move the caret (arrow keys first) and only then \
         confirm: {planned:?}"
    );

    // State-machine shape: the first bytes actually written to the PTY must
    // move the caret, never confirm the highlighted entry outright.
    let mut seq = StartupSeq::with_idle_gap(b"What is 2+2?".to_vec(), 100);
    assert!(
        matches!(seq.feed(&capture), StartupAction::None),
        "keys must be held while the TUI paints"
    );
    std::thread::sleep(Duration::from_millis(450));
    match seq.poll_timers() {
        StartupAction::Write(keys) => assert!(
            keys.starts_with(b"\x1b["),
            "blind Enter regression: the first write must move the caret off \
             'No, exit' before confirming, got {keys:?}"
        ),
        other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
    }
    assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
}

/// An unidentifiable dialog (no Yes/No wording to classify) must produce
/// Refuse once the screen settles — never a confirming keystroke, which would
/// select 2.1.263's highlighted "No, exit" default.
#[test]
fn startup_unidentifiable_dialog_refuses_rather_than_guessing() {
    let dialog = concat!(
        "Quick safety check: trust this folder before you continue?\r\n",
        "\u{276f} Depart\r\n",
        "  Remain\r\n",
        "Enter to confirm \u{b7} Esc to cancel\r\n",
    );
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    assert!(
        matches!(seq.feed(dialog.as_bytes()), StartupAction::None),
        "nothing may be sent before the screen settles"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);

    // Screen settled (DIALOG_SETTLE_MS = 2 s) with no identifiable trusting entry.
    std::thread::sleep(Duration::from_millis(2_100));
    match seq.poll_timers() {
        StartupAction::Refuse(reason) => {
            assert!(
                reason.contains("--pretrust-cwd"),
                "refusal must be actionable: {reason}"
            );
        }
        other => panic!("expected Refuse for an unidentifiable dialog, got {other:?}"),
    }
    assert_eq!(
        *seq.phase(),
        StartupPhase::Waiting,
        "a refusal must not advance the phase or confirm any entry"
    );
}

// ── Token count regression: fixture transcript_v2.1.168.jsonl ─────────────────

#[test]
fn token_regression_fixture_v2_1_168() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_v2.1.168.jsonl");
    let r = parse_transcript(&path).expect("parse fixture failed");
    // Turn 1: msg-001 — in=6178, out=295, cache_create=825, cache_read=26442
    // Turn 2: msg-002 × 3 streaming chunks — in=100, out=50, cache_create=0, cache_read=5000
    assert_eq!(r.num_turns, 2, "fixture has 2 unique assistant turns");
    assert_eq!(
        r.usage.input_tokens, 6278,
        "input_tokens mismatch (6178 + 100)"
    );
    assert_eq!(
        r.usage.output_tokens, 345,
        "output_tokens mismatch (295 + 50)"
    );
    assert_eq!(
        r.usage.cache_creation_input_tokens, 825,
        "cache_creation mismatch (825 + 0)"
    );
    assert_eq!(
        r.usage.cache_read_input_tokens, 31442,
        "cache_read mismatch (26442 + 5000)"
    );
    // Last turn's text is the concatenation of the 3 streaming chunks
    assert_eq!(r.text, "chunk1 chunk2 chunk3", "last turn text mismatch");
}

// ── Fixture also contains unknown usage fields → ignored ──────────────────────

#[test]
fn fixture_unknown_usage_fields_ignored() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_v2.1.168.jsonl");
    // The fixture contains `server_tool_use`, `service_tier`, `cache_creation`,
    // `inference_geo`, `speed` in the usage object — all should be silently ignored.
    let r = parse_transcript(&path).expect("parse fixture must succeed");
    assert!(
        r.num_turns > 0,
        "must parse at least one turn despite unknown usage fields"
    );
}

// ── Token count regression: fixture transcript_v2.1.233.jsonl ─────────────────

#[test]
fn token_regression_fixture_v2_1_233() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_v2.1.233.jsonl");
    let r = parse_transcript(&path).expect("parse fixture failed");
    // Turn 1: msg-101 — in=5200, out=245, cache_create=650, cache_read=21000
    // Turn 2: msg-102 × 2 streaming chunks — in=150, out=80 (first chunk), cache_create=0, cache_read=6000
    // Turn 3: msg-103 — in=200, out=95, cache_create=0, cache_read=7500
    assert_eq!(r.num_turns, 3, "fixture has 3 unique assistant turns");
    assert_eq!(
        r.usage.input_tokens, 5550,
        "input_tokens mismatch (5200 + 150 + 200)"
    );
    assert_eq!(
        r.usage.output_tokens, 420,
        "output_tokens mismatch (245 + 80 + 95)"
    );
    assert_eq!(
        r.usage.cache_creation_input_tokens, 650,
        "cache_creation mismatch (650 + 0 + 0)"
    );
    assert_eq!(
        r.usage.cache_read_input_tokens, 34500,
        "cache_read mismatch (21000 + 6000 + 7500)"
    );
    // Last turn's text
    assert_eq!(r.text, "Final answer", "last turn text mismatch");
}

// ── Fixture v2.1.233 also contains unknown usage fields → ignored ─────────────

#[test]
fn fixture_v233_unknown_usage_fields_ignored() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transcript_v2.1.233.jsonl");
    // The fixture contains `server_tool_use`, `service_tier`, `cache_creation`,
    // `inference_geo`, `speed` in the usage object — all should be silently ignored.
    let r = parse_transcript(&path).expect("parse fixture must succeed");
    assert!(
        r.num_turns > 0,
        "must parse at least one turn despite unknown usage fields"
    );
}
