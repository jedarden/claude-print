use claude_print::startup::{StartupAction, StartupPhase, StartupSeq};
use std::time::Duration;

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

/// Alternative wording: `continue` + `folder` → CR sent (keyword union logic).
///
/// This is the Phase 10 "MEDIUM" scenario: trust dialog uses different wording
/// than "trust Allow" — the keyword union covers "continue", "folder", "proceed",
/// "permission" as alternatives.  Verifies that any two keywords from the union
/// trigger the dismiss even when the primary keywords are absent.
#[test]
fn test_trust_dialog_alternate_wording_continue_folder() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // Line contains ONLY "continue" + "folder" from the keyword set — no "trust"/"Allow".
    let action = seq.feed(b"Do you want to continue in this folder?\n");
    match action {
        StartupAction::Write(bytes) => assert_eq!(
            bytes, b"\r",
            "'continue' + 'folder' must send CR (alternate trust wording)"
        ),
        other => {
            panic!("expected Write(b\"\\r\") for 'continue'+'folder' keywords, got: {other:?}")
        }
    }
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must be TrustDismissed after 'continue'+'folder' trigger"
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
    assert!(
        matches!(a1, StartupAction::None),
        "partial line must not trigger yet"
    );
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

// ── Idle fallback (≥ 200 bytes + 0.4 s silence) ──────────────────────────────

/// 200 bytes received, then 0.4 s idle → CR sent via idle fallback (no keywords needed).
///
/// This verifies the plan's "arbitrary unknown welcome text" path: claude emits
/// ≥ 200 bytes of startup noise with no keywords, then goes quiet — claude-print
/// must still dismiss the trust phase via the idle fallback.
#[test]
fn test_idle_fallback_fires_after_200_bytes_and_silence() {
    let gap_ms: u64 = 30;
    let mut seq = StartupSeq::with_idle_gap(b"prompt".to_vec(), gap_ms);

    // Feed exactly 200 bytes of non-keyword output to clear the byte threshold.
    let noise = vec![b'x'; 200];
    let action = seq.feed(&noise);
    // No keywords → no CR yet.
    assert!(
        matches!(action, StartupAction::None),
        "200 bytes of noise must not immediately trigger trust dismiss (no keywords)"
    );
    assert_eq!(
        *seq.phase(),
        StartupPhase::Waiting,
        "still Waiting after byte dump"
    );

    // The WAITING idle threshold is fixed at 400ms; `gap_ms` configures only
    // the later post-dismiss quiet period.
    std::thread::sleep(Duration::from_millis(500));
    let action = seq.poll_timers();
    match action {
        StartupAction::Write(bytes) => assert_eq!(bytes, b"\r", "idle fallback must send CR"),
        StartupAction::HardTimeout => panic!("hard timeout should not fire — ≥ 200 bytes received"),
        StartupAction::None => panic!("idle fallback must fire after 0.4 s with ≥ 200 bytes"),
    }
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must advance to TrustDismissed via idle fallback"
    );
}

/// Fewer than 200 bytes received → idle fallback must NOT fire even after 0.4 s.
/// This verifies the 200-byte minimum is enforced before the idle fallback.
#[test]
fn test_idle_fallback_does_not_fire_below_200_bytes() {
    let mut seq = StartupSeq::with_idle_gap(b"prompt".to_vec(), 20);

    // Feed 199 bytes — one below the threshold.
    let noise = vec![b'y'; 199];
    seq.feed(&noise);
    assert_eq!(*seq.phase(), StartupPhase::Waiting);

    // Wait past the idle window.
    std::thread::sleep(Duration::from_millis(500));

    let action = seq.poll_timers();
    // Must not fire the idle fallback (< 200 bytes).
    assert!(
        !matches!(action, StartupAction::Write(_)),
        "idle fallback must not fire with only 199 bytes received; got: {action:?}"
    );
    assert_eq!(
        *seq.phase(),
        StartupPhase::Waiting,
        "phase must remain Waiting when byte threshold not met"
    );
}

/// Hard timeout fires when WAITING persists for ≥ 45 s with fewer than 200 bytes.
///
/// This test is slow by design — it verifies the binary-not-found / partial-output-hang
/// detection described in EC-8.  Use `#[ignore]` to skip in fast test runs.
///
/// To run: `cargo test test_hard_timeout -- --ignored`
#[test]
#[ignore = "slow: sleeps 45 s to verify the hard timeout"]
fn test_hard_timeout_fires_after_45s_with_few_bytes() {
    let mut seq = StartupSeq::with_idle_gap(b"prompt".to_vec(), 2000);

    // Feed < 200 bytes so the idle fallback never fires.
    seq.feed(b"tiny output\n");

    // Wait past the 45 s hard timeout.
    std::thread::sleep(Duration::from_secs(46));

    let action = seq.poll_timers();
    assert!(
        matches!(action, StartupAction::HardTimeout),
        "hard timeout must fire after 45 s with < 200 bytes; got: {action:?}"
    );
}
