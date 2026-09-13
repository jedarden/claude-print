use claude_print::startup::{StartupAction, StartupPhase, StartupSeq};
use std::time::Duration;

/// The trust dialog exactly as claude 2.1.263 renders it in a cwd that has not
/// been trusted yet, screen-scraped from a real TUI under a PTY
/// (claudepr-fe3d3160). The REFUSING entry is highlighted by default, so the
/// dismissal must move the caret onto the trusting entry before pressing Enter.
const DIALOG_CARET_ON_REFUSE: &str = concat!(
    "Quick safety check: Is this a project you created or one you trust?\r\n",
    "\u{276f} No, exit\r\n",
    "  Yes, I trust this folder\r\n",
    "Enter to confirm \u{b7} Esc to cancel\r\n",
);

/// The pre-2.1.263 layout: the trusting entry was highlighted by default.
const DIALOG_CARET_ON_TRUST: &str = concat!(
    "Do you trust the files in this folder?\r\n",
    "\u{276f} Yes, proceed\r\n",
    "  No, exit\r\n",
);

/// Drive an identified dismissal to the TrustDismissed phase and assert the
/// exact key sequence.
///
/// The sequencer deliberately holds identified keys until the TUI has been
/// quiet for DISMISS_SETTLE_MS (400 ms) — keys written while the TUI is
/// still painting its first frame are silently dropped (claudepr-fe3d3160) —
/// so `feed` returns `None` at the parseable instant and the keys only come
/// out of `poll_timers` once the quiet window has elapsed.
fn dismiss_and_assert(seq: &mut StartupSeq, expected: &[u8]) {
    std::thread::sleep(std::time::Duration::from_millis(450));
    match seq.poll_timers() {
        StartupAction::Write(keys) => assert_eq!(
            keys, expected,
            "dismissal must send exactly {expected:?} after the quiet window"
        ),
        other => panic!("expected dismissal keys {expected:?}, got: {other:?}"),
    }
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must advance to TrustDismissed once the keys are sent"
    );
}

// ── Trust dialog keyword detection ───────────────────────────────────────────

/// Two trust keywords on one line detect the dialog but never send keys on
/// their own: the highlighted entry is unknown until the option list renders.
#[test]
fn test_trust_dialog_keyword_match_detects_dialog_without_keys() {
    let mut seq = StartupSeq::new(b"What is 2+2?".to_vec());
    let action = seq.feed(b"Do you trust and Allow this folder?\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "a bare CR must not be sent before the option list renders (claudepr-fe3d3160)"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
}

/// Completing the same dialog (options with the caret on the refusing entry)
/// moves the caret to the trusting entry, then confirms.
#[test]
fn test_trust_dialog_keyword_match_dismisses_once_options_render() {
    let mut seq = StartupSeq::new(b"What is 2+2?".to_vec());
    seq.feed(b"Do you trust and Allow this folder?\r\n");
    let options = DIALOG_CARET_ON_REFUSE.replace(
        "Quick safety check: Is this a project you created or one you trust?\r\n",
        "",
    );
    let action = seq.feed(options.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are identified but held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// Exactly two keywords on the same line detect the dialog (boundary check).
#[test]
fn test_trust_dialog_keyword_threshold_two_triggers() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // "trust" + "proceed" = exactly 2 → dialog detected, but still no keys
    // until the trusting entry is identified.
    let action = seq.feed(b"trust this and proceed\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "keywords alone must detect the dialog, not dismiss it"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
}

/// Alternative wording: `continue` + `folder` → dialog detected (keyword union
/// logic), dismissed only once the trusting entry renders.
///
/// This is the Phase 10 "MEDIUM" scenario: trust dialog uses different wording
/// than "trust Allow" — the keyword union covers "continue", "folder", "proceed",
/// "permission" as alternatives.
#[test]
fn test_trust_dialog_alternate_wording_continue_folder() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // Line contains ONLY "continue" + "folder" from the keyword set — no "trust"/"Allow".
    let action = seq.feed(b"Do you want to continue in this folder?\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "'continue'+'folder' must detect the dialog without dismissing it"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);

    // The trusting entry renders below the caret → down, then Enter.
    let options = DIALOG_CARET_ON_REFUSE.replace(
        "Quick safety check: Is this a project you created or one you trust?\r\n",
        "",
    );
    let action = seq.feed(options.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "identified keys are held until the quiet window, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// A single keyword never detects a dialog (< 2 threshold).
#[test]
fn test_trust_dialog_single_keyword_no_trigger() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(b"Please proceed with the next step\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "single keyword must not trigger dismiss"
    );
}

/// No keywords → no trigger.
#[test]
fn test_trust_dialog_no_keywords_no_trigger() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(b"Initializing Claude Code v2.1.168...\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "no keywords must not trigger dismiss"
    );
}

/// Phase transitions to TrustDismissed after the caret is moved onto the
/// trusting entry and Enter is sent.
#[test]
fn test_trust_dialog_phase_becomes_trust_dismissed() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
    let action = seq.feed(DIALOG_CARET_ON_REFUSE.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// The dismissal is one-shot: once TrustDismissed, further dialog output is
/// ignored.
#[test]
fn test_trust_dialog_dismiss_is_one_shot() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First trigger.
    seq.feed(DIALOG_CARET_ON_REFUSE.as_bytes());
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
    // A second trust-dialog render must not produce more keys.
    let action = seq.feed(b"trust Allow folder permission proceed continue\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "second keyword match after dismiss must be ignored"
    );
}

/// The question line and the option list may arrive in separate chunks — the
/// dismissal is identified as soon as the trusting entry is on screen.
#[test]
fn test_trust_dialog_options_across_chunk_boundary() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First chunk: partial dialog — question only.
    let a1 = seq.feed(b"Quick safety check: Is this a project you created or one you trust?\r\n");
    assert!(
        matches!(a1, StartupAction::None),
        "partial dialog must not trigger yet"
    );
    // Second chunk: refusing entry with the caret — still not enough.
    let a2 = seq.feed("\u{276f} No, exit\r\n".as_bytes());
    assert!(
        matches!(a2, StartupAction::None),
        "caret on the refusing entry alone must not dismiss"
    );
    // Third chunk: the trusting entry completes the dialog — identified, but
    // held for the quiet window.
    let a3 = seq.feed("  Yes, I trust this folder\r\n".as_bytes());
    assert!(
        matches!(a3, StartupAction::None),
        "keys are held until the TUI has been quiet, got {a3:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// A benign line before the dialog does not disturb detection.
#[test]
fn test_trust_dialog_on_second_block() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // First block: no keywords, no options.
    let a1 = seq.feed(b"Loading configuration...\r\n");
    assert!(matches!(a1, StartupAction::None));
    // Second block: the dialog.
    let a2 = seq.feed(DIALOG_CARET_ON_REFUSE.as_bytes());
    assert!(
        matches!(a2, StartupAction::None),
        "keys are held while the TUI may still be painting, got {a2:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// CRLF-terminated dialog lines are handled like LF ones.
#[test]
fn test_trust_dialog_crlf_terminated_dialog() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(DIALOG_CARET_ON_TRUST.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "identified keys are held until the quiet window, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\r");
}

/// Keyword matching is case-sensitive: "allow" (lowercase) does not match "Allow".
#[test]
fn test_trust_dialog_case_sensitive_keywords() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    // "allow" (lowercase) is not in the keyword list; only "trust" matches → 1 keyword.
    let action = seq.feed(b"allow me to trust this\r\n");
    assert!(
        matches!(action, StartupAction::None),
        "lowercase 'allow' must not count as the 'Allow' keyword"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
}

// ── Caret placement (claudepr-fe3d3160) ──────────────────────────────────────

/// The headline regression: claude 2.1.263 highlights "No, exit" first, so the
/// dismissal must move the caret to the trusting entry before Enter.
#[test]
fn test_caret_on_refuse_entry_moves_down_before_enter() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(DIALOG_CARET_ON_REFUSE.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// Older layouts highlighted the trusting entry: a plain Enter is correct there.
#[test]
fn test_caret_on_trusting_entry_confirms_in_place() {
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(DIALOG_CARET_ON_TRUST.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\r");
}

/// Entry order must not be assumed: when the trusting entry sits *above* the
/// caret, the caret moves up.
#[test]
fn test_caret_below_trusting_entry_moves_up() {
    let dialog = concat!(
        "Do you trust the files in this folder?\r\n",
        "  Yes, proceed\r\n",
        "\u{276f} No, exit\r\n",
    );
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(dialog.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[A\r");
}

/// A repaint of the same dialog must not confuse the caret position: the newest
/// render wins.
#[test]
fn test_redraw_updates_caret_position() {
    let mut capture = DIALOG_CARET_ON_REFUSE.to_string();
    capture.push_str("\x1b[2K  No, exit\r\n\x1b[2K\u{276f} Yes, I trust this folder\r\n");
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(capture.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\r");
}

/// The real TUI colourises the caret and the highlighted entry; escape
/// sequences must not defeat the match.
#[test]
fn test_ansi_colored_dialog_still_parses() {
    let dialog = concat!(
        "Quick safety check: Is this a project you created or one you trust?\r\n",
        "\x1b[1;36m\u{276f}\x1b[0m \x1b[36mNo, exit\x1b[0m\r\n",
        "  Yes, I trust this folder\r\n",
        "Enter to confirm \u{b7} Esc to cancel\r\n",
    );
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    let action = seq.feed(dialog.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
}

/// A future dialog whose entries match neither Yes nor No must never be
/// confirmed by a bare CR — the default may be the refusing entry.
#[test]
fn test_unrecognised_entries_are_never_confirmed() {
    let dialog = concat!(
        "Quick safety check: trust this folder before you continue?\r\n",
        "\u{276f} Depart\r\n",
        "  Remain\r\n",
        "Enter to confirm \u{b7} Esc to cancel\r\n",
    );
    assert!(StartupSeq::dialog_present(dialog.as_bytes()));
    assert!(StartupSeq::plan_keys(dialog.as_bytes()).is_none());

    let mut seq = StartupSeq::new(b"prompt".to_vec());
    seq.feed(dialog.as_bytes());
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
    // The dialog is never dismissed by a guess: once the screen has settled
    // (2 s of silence) the session refuses loudly instead.
    std::thread::sleep(Duration::from_millis(2100));
    match seq.poll_timers() {
        StartupAction::Refuse(reason) => {
            assert!(
                reason.contains("--pretrust-cwd"),
                "refusal must name the escape hatch: {reason}"
            );
            assert!(
                reason.contains("Depart"),
                "refusal must quote what the TUI actually rendered: {reason}"
            );
        }
        other => panic!("expected Refuse, got: {other:?}"),
    }
}

/// Options without a visible caret: the highlighted entry is unknown, so
/// nothing may be confirmed — the trusting entry being on screen is not enough
/// to dismiss without knowing where the caret sits.
#[test]
fn test_dialog_without_caret_is_never_confirmed() {
    let dialog = concat!(
        "Quick safety check: Is this a project you created or one you trust?\r\n",
        "  No, exit\r\n",
        "  Yes, I trust this folder\r\n",
        "Enter to confirm \u{b7} Esc to cancel\r\n",
    );
    assert!(StartupSeq::dialog_present(dialog.as_bytes()));
    assert!(StartupSeq::plan_keys(dialog.as_bytes()).is_none());

    // Through the state machine: no bare CR may fire while the dialog sits
    // unresolved, and the settled screen ends in a loud Refuse.
    let mut seq = StartupSeq::new(b"prompt".to_vec());
    seq.feed(dialog.as_bytes());
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
    std::thread::sleep(Duration::from_millis(2100));
    match seq.poll_timers() {
        StartupAction::Refuse(reason) => {
            assert!(
                reason.contains("--pretrust-cwd"),
                "refusal must name the escape hatch: {reason}"
            );
            assert!(
                reason.contains("Yes, I trust this folder"),
                "refusal must quote what the TUI actually rendered: {reason}"
            );
        }
        other => panic!("expected Refuse without a visible caret, got: {other:?}"),
    }
}

// ── Prompt injection payload ─────────────────────────────────────────────────

/// After dismissal the injected payload uses bracketed paste markers.
#[test]
fn test_trust_dialog_prompt_payload_uses_bracketed_paste() {
    let prompt = b"What is 2+2?";
    let mut seq = StartupSeq::new(prompt.to_vec());
    // Dismiss: keys are identified on feed but held for the quiet window.
    let action = seq.feed(DIALOG_CARET_ON_REFUSE.as_bytes());
    assert!(
        matches!(action, StartupAction::None),
        "keys are held while the TUI may still be painting, got {action:?}"
    );
    dismiss_and_assert(&mut seq, b"\x1b[B\r");
    // The payload format itself (bracketed-paste envelope + CR around the
    // verbatim prompt bytes) is covered by the unit-level make_prompt_payload
    // tests in src/startup.rs; this test pins the state-machine sequence only.
    assert_eq!(
        *seq.phase(),
        StartupPhase::TrustDismissed,
        "phase must be TrustDismissed before the idle-gap timer fires"
    );
}

// ── Idle fallback (≥ 200 bytes + 0.4 s silence) ──────────────────────────────

/// 200 bytes received, then 0.4 s idle → CR sent via idle fallback (no dialog).
///
/// This verifies the plan's "arbitrary unknown welcome text" path: in a trusted
/// cwd claude emits ≥ 200 bytes of startup noise with no trust dialog at all,
/// then goes quiet — claude-print must still leave the waiting phase via the
/// idle fallback.
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
        StartupAction::Refuse(reason) => {
            panic!("no dialog is on screen — must not refuse: {reason}")
        }
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

/// A dialog on screen suppresses the idle fallback: 0.4 s of quiet must not
/// send a bare CR that would confirm the highlighted "No, exit".
#[test]
fn test_idle_fallback_suppressed_while_dialog_is_up() {
    let mut seq = StartupSeq::with_idle_gap(b"prompt".to_vec(), 20);

    let dialog = DIALOG_CARET_ON_REFUSE.replace("  Yes, I trust this folder\r\n", "");
    seq.feed(dialog.as_bytes());
    assert_eq!(*seq.phase(), StartupPhase::Waiting);

    // Well past both the 400 ms idle fallback and the 200-byte floor.
    std::thread::sleep(Duration::from_millis(500));
    let action = seq.poll_timers();
    let bare_cr = matches!(&action, StartupAction::Write(bytes) if bytes == b"\r");
    assert!(
        !bare_cr,
        "idle fallback must not bare-confirm a dialog that cannot be resolved; got: {action:?}"
    );
    assert_eq!(*seq.phase(), StartupPhase::Waiting);
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
