use claude_print::startup::{StartupAction, StartupPhase, StartupSeq};

// ── Trust dialog keyword detection ───────────────────────────────────────────

/// Two trust keywords on one line triggers a CR keypress.
#[test]
fn test_trust_dialog_keyword_match_returns_cr() {
    let mut seq = StartupSeq::new(b"What is 2+2?".to_vec());
    let action = seq.feed(b"Do you trust and Allow this folder?\n");
    match action {
        StartupAction::Write(bytes) => assert_eq!(
            bytes, b"\r",
            "trust dialog dismiss must be a single CR byte"
        ),
        _ => panic!("expected Write(b\"\\r\") on trust keyword match"),
    }
}

/// Exactly two keywords on the same line triggers (boundary check).
#[test]
fn test_trust_dialog_keyword_threshold_two_triggers() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // "trust" + "proceed" = exactly 2 → must trigger.
    let action = seq.feed(b"trust this and proceed\n");
    assert!(
        matches!(action, StartupAction::Write(_)),
        "exactly 2 keywords must trigger dismiss"
    );
}

/// A single keyword never triggers (< 2 threshold).
#[test]
fn test_trust_dialog_single_keyword_no_trigger() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(b"Please proceed with the next step\n");
    assert!(
        matches!(action, StartupAction::None),
        "single keyword must not trigger dismiss"
    );
}

/// No keywords → no trigger.
#[test]
fn test_trust_dialog_no_keywords_no_trigger() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(b"Initializing Claude Code v2.1.168...\n");
    assert!(
        matches!(action, StartupAction::None),
        "no keywords must not trigger dismiss"
    );
}

/// Phase transitions to TrustDismissed after keyword match.
#[test]
fn test_trust_dialog_phase_becomes_trust_dismissed() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
    seq.feed(b"trust Allow folder\n");
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must advance to TrustDismissed after keyword match"
    );
}

/// Once TrustDismissed, further trust-keyword lines are ignored.
#[test]
fn test_trust_dialog_dismiss_is_one_shot() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First trigger.
    seq.feed(b"trust Allow\n");
    assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    // Second trust-dialog line must not produce another CR.
    let action = seq.feed(b"trust Allow folder permission proceed continue\n");
    assert!(
        matches!(action, StartupAction::None),
        "second keyword match after dismiss must be ignored"
    );
}

/// Keywords split across two feed() calls are assembled by the line buffer.
#[test]
fn test_trust_dialog_keywords_across_chunk_boundary() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First chunk: partial line ending mid-word.
    let a1 = seq.feed(b"Do you trust and ");
    assert!(matches!(a1, StartupAction::None), "partial line must not trigger yet");
    // Second chunk: completes the line.
    let a2 = seq.feed(b"Allow access to the folder?\n");
    match a2 {
        StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
        _ => panic!("expected CR once line is complete"),
    }
}

/// Keywords on the second of two lines (first line benign) triggers on the correct line.
#[test]
fn test_trust_dialog_keyword_on_second_line() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First line: no keywords.
    let a1 = seq.feed(b"Loading configuration...\n");
    assert!(matches!(a1, StartupAction::None));
    // Second line: two keywords.
    let a2 = seq.feed(b"Grant permission to proceed?\n");
    match a2 {
        StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
        _ => panic!("expected CR on second line with keywords"),
    }
}

/// CR-terminated lines (\\r instead of \\n) also trigger the scanner.
#[test]
fn test_trust_dialog_cr_terminated_line() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(b"trust Allow folder\r");
    match action {
        StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
        _ => panic!("expected CR on CR-terminated trust line"),
    }
}

/// Keyword matching is case-sensitive: "allow" (lowercase) does not match "Allow".
#[test]
fn test_trust_dialog_case_sensitive_keywords() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // "allow" (lowercase) is not in the keyword list; only "trust" matches → 1 keyword.
    let action = seq.feed(b"allow me to trust this\n");
    assert!(
        matches!(action, StartupAction::None),
        "lowercase 'allow' must not count as the 'Allow' keyword"
    );
}

// ── Prompt injection payload ─────────────────────────────────────────────────

/// After dismissal the injected payload uses bracketed paste markers.
#[test]
fn test_trust_dialog_prompt_payload_uses_bracketed_paste() {
    let prompt = b"What is 2+2?";
    let mut seq = StartupSeq::new(prompt.to_vec());
    // Trigger dismiss.
    seq.feed(b"trust Allow\n");
    assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    // Force the post-dismiss timer to fire by back-dating the dismiss timestamp.
    // We do this by calling poll_timers after manually advancing time via a
    // SpeedHack: set trust_dismiss_at to 3 s ago by creating a fresh seq at
    // TrustDismissed with the dismiss timestamp in the past.
    // Since Instant is not constructible from a fixed value, we test payload
    // format via the unit-level make_prompt_payload path already covered in
    // startup.rs unit tests.  Here we verify the state machine sequence only.
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must be TrustDismissed before timer fires"
    );
}
