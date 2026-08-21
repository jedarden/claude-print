use std::time::{Duration, Instant};

// Trust dialog keyword set — 2+ on a single line → send CR.
const TRUST_KEYWORDS: &[&str] = &[
    "trust",
    "Allow",
    "continue",
    "folder",
    "permission",
    "proceed",
];
const KEYWORD_THRESHOLD: usize = 2;

const IDLE_THRESHOLD_BYTES: usize = 200;
/// Quiet period required before the trust-dialog fallback dismisses unknown output.
const IDLE_TIMEOUT_MS: u64 = 400;
const HARD_TIMEOUT_SECS: u64 = 45;
/// Default idle-gap: ms of silence after trust-dismiss before injecting prompt.
/// Resets to zero on every PTY output chunk; fires only after uninterrupted silence.
pub const DEFAULT_POST_DISMISS_IDLE_MS: u64 = 1000;

/// Action requested by [`StartupSeq`] from the event loop.
#[derive(Debug)]
pub enum StartupAction {
    /// Write these bytes to the PTY master fd.
    Write(Vec<u8>),
    /// No action needed this iteration.
    None,
    /// Hard timeout fired (≤ 200 bytes in 45 s) — caller should SIGTERM child and exit 2.
    HardTimeout,
}

/// Phase of the startup sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum StartupPhase {
    /// Waiting for trust dialog keywords or idle fallback.
    Waiting,
    /// CR was sent to dismiss trust dialog; waiting for quiet before injection.
    TrustDismissed,
    /// Bracketed paste was sent; waiting for the Stop hook.
    PromptInjected,
}

impl StartupPhase {
    /// Returns true if the phase is PromptInjected (prompt has been sent to the child).
    pub fn is_prompt_injected(&self) -> bool {
        matches!(self, Self::PromptInjected)
    }
}

/// Manages the startup handshake with the Claude Code TUI.
///
/// Phase 1: scan PTY output for trust-dialog keywords; send `\r` to dismiss.
/// Phase 2: wait for an idle gap (no PTY output for `idle_gap_ms`), then inject
///          the user prompt via bracketed paste.  The idle gap resets on every
///          output chunk so transient TUI redraws after the dismiss don't cause
///          premature injection.
///
/// Call [`feed`] for every PTY chunk and [`poll_timers`] on each poll() iteration.
pub struct StartupSeq {
    phase: StartupPhase,
    prompt: Vec<u8>,
    bytes_received: usize,
    /// Timestamp of the most-recent PTY output, or the dismiss instant when
    /// entering TrustDismissed via the idle-fallback path.  Used as the start
    /// of the idle-gap window.
    last_output_at: Instant,
    phase_start: Instant,
    trust_dismiss_at: Option<Instant>,
    /// Accumulates bytes from the current partial line for keyword scanning.
    line_buf: Vec<u8>,
    /// Configurable idle gap (ms).  After trust-dismiss, injection fires only
    /// after this many ms pass with no PTY output.
    idle_gap_ms: u64,
}

impl StartupSeq {
    pub fn new(prompt: Vec<u8>) -> Self {
        Self::with_idle_gap(prompt, DEFAULT_POST_DISMISS_IDLE_MS)
    }

    /// Construct with a custom post-dismiss idle gap in milliseconds.
    ///
    /// Primarily used in tests to avoid waiting for the default quiet period.
    pub fn with_idle_gap(prompt: Vec<u8>, idle_gap_ms: u64) -> Self {
        let now = Instant::now();
        Self {
            phase: StartupPhase::Waiting,
            prompt,
            bytes_received: 0,
            last_output_at: now,
            phase_start: now,
            trust_dismiss_at: None,
            line_buf: Vec::new(),
            idle_gap_ms,
        }
    }

    pub fn phase(&self) -> &StartupPhase {
        &self.phase
    }

    /// Returns `true` if `line` contains ≥ 2 trust-dialog keywords.
    ///
    /// Matching is byte-exact (same case as the keyword list) to avoid
    /// false positives on common words like "allow" (lowercase).
    pub fn scan_line(line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line);
        let count = TRUST_KEYWORDS.iter().filter(|&&k| text.contains(k)).count();
        count >= KEYWORD_THRESHOLD
    }

    /// Feed a chunk of PTY output.
    ///
    /// Scans for trust keywords line-by-line.  Returns [`StartupAction::Write`]
    /// containing `b"\r"` on the first line that matches; no-ops in all other phases.
    /// Call [`poll_timers`] separately to handle deadline-driven transitions.
    pub fn feed(&mut self, chunk: &[u8]) -> StartupAction {
        self.feed_at(chunk, Instant::now())
    }

    fn feed_at(&mut self, chunk: &[u8], now: Instant) -> StartupAction {
        self.bytes_received += chunk.len();
        self.last_output_at = now;

        if self.phase != StartupPhase::Waiting {
            return StartupAction::None;
        }

        for &b in chunk {
            if b == b'\n' || b == b'\r' {
                if Self::scan_line(&self.line_buf) {
                    self.line_buf.clear();
                    self.phase = StartupPhase::TrustDismissed;
                    self.trust_dismiss_at = Some(now);
                    return StartupAction::Write(b"\r".to_vec());
                }
                self.line_buf.clear();
            } else {
                self.line_buf.push(b);
            }
        }

        StartupAction::None
    }

    /// Poll deadline-driven transitions.  Call once per poll() iteration.
    ///
    /// Handles:
    /// - Hard timeout (WAITING, < 200 bytes in 45 s) → [`StartupAction::HardTimeout`]
    /// - Idle fallback (WAITING, ≥ 200 bytes, 0.4 s quiet) → CR write
    /// - Post-dismiss idle gap (TRUST_DISMISSED, no output for `idle_gap_ms`) → bracketed paste
    ///
    /// Both transitions require uninterrupted silence. Every PTY chunk received
    /// via [`feed`] restarts the current quiet window, so a settled TUI advances
    /// after 0.4 s / 1.0 s while an actively rendering TUI continues to wait.
    pub fn poll_timers(&mut self) -> StartupAction {
        self.poll_timers_at(Instant::now())
    }

    fn poll_timers_at(&mut self, now: Instant) -> StartupAction {
        match self.phase {
            StartupPhase::Waiting => {
                if now.duration_since(self.phase_start) >= Duration::from_secs(HARD_TIMEOUT_SECS)
                    && self.bytes_received < IDLE_THRESHOLD_BYTES
                {
                    return StartupAction::HardTimeout;
                }

                if self.bytes_received >= IDLE_THRESHOLD_BYTES
                    && now.duration_since(self.last_output_at)
                        >= Duration::from_millis(IDLE_TIMEOUT_MS)
                {
                    // Reset last_output_at so the next quiet window starts at the
                    // dismiss moment, not at the final Waiting-phase output.
                    self.last_output_at = now;
                    self.phase = StartupPhase::TrustDismissed;
                    self.trust_dismiss_at = Some(now);
                    return StartupAction::Write(b"\r".to_vec());
                }

                StartupAction::None
            }

            StartupPhase::TrustDismissed => {
                if now.duration_since(self.last_output_at)
                    >= Duration::from_millis(self.idle_gap_ms)
                {
                    let payload = self.make_prompt_payload();
                    self.phase = StartupPhase::PromptInjected;
                    return StartupAction::Write(payload);
                }
                StartupAction::None
            }

            StartupPhase::PromptInjected => StartupAction::None,
        }
    }

    /// Build the bracketed-paste payload that injects `self.prompt` into the
    /// Claude Code REPL.
    ///
    /// The prompt bytes are delivered *verbatim*, wrapped in a single
    /// bracketed-paste envelope (`ESC[200~` … `ESC[201~`) terminated by `CR`.
    /// Bracketed paste makes embedded newlines literal (no premature Enter),
    /// and — critically — its content is inserted into the Ink REPL as-is:
    /// there is no shell on the paste path to evaluate any command
    /// substitution.  An earlier revision emitted `$(< <tmpfile>)` here, which
    /// the model received as the literal string rather than the file contents
    /// (bf-4rxh); the actual prompt text must therefore be carried in the
    /// payload itself, regardless of size.
    ///
    /// Large payloads (which can far exceed the kernel PTY/pipe buffer) are
    /// fully drained by the caller's chunked write loop (`write_pty_all` in
    /// `session.rs`); a single `write(2)` would short-write and silently
    /// truncate the prompt.
    fn make_prompt_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.prompt.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(&self.prompt);
        out.extend_from_slice(b"\x1b[201~\r");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── scan_line unit tests ──────────────────────────────────────────────────

    #[test]
    fn scan_line_two_keywords_returns_true() {
        assert!(StartupSeq::scan_line(
            b"Do you trust and Allow this folder?"
        ));
    }

    #[test]
    fn scan_line_single_keyword_returns_false() {
        assert!(!StartupSeq::scan_line(b"Press enter to proceed"));
    }

    #[test]
    fn scan_line_empty_returns_false() {
        assert!(!StartupSeq::scan_line(b""));
    }

    #[test]
    fn scan_line_all_keywords_returns_true() {
        assert!(StartupSeq::scan_line(
            b"trust Allow continue folder permission proceed"
        ));
    }

    #[test]
    fn scan_line_case_sensitive_allow_lowercase_not_matched() {
        // "allow" (lowercase) does not match the "Allow" keyword.
        // Only one keyword ("trust") → should not trigger.
        assert!(!StartupSeq::scan_line(b"allow me to trust you"));
    }

    // ── feed() unit tests ─────────────────────────────────────────────────────

    #[test]
    fn feed_trust_line_returns_cr_byte() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        // Line contains "trust" + "Allow" (2 keywords) → must return CR.
        let action = seq.feed(b"Do you trust and Allow this folder?\n");
        match action {
            StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
            _ => panic!("expected Write(b\"\\r\")"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    #[test]
    fn feed_no_keywords_returns_none() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let action = seq.feed(b"Starting Claude Code...\n");
        assert!(matches!(action, StartupAction::None));
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
    }

    #[test]
    fn feed_single_keyword_no_trigger() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let action = seq.feed(b"Press enter to proceed\n");
        assert!(matches!(action, StartupAction::None));
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
    }

    #[test]
    fn feed_trust_dismissed_phase_ignored() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        // Trigger dismiss first.
        seq.feed(b"trust Allow folder\n");
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
        // Additional output in TrustDismissed phase must be ignored.
        let action = seq.feed(b"trust Allow folder permission proceed\n");
        assert!(matches!(action, StartupAction::None));
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    #[test]
    fn feed_keywords_split_across_chunks_trigger_on_newline() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        // First chunk: partial line with first keyword.
        let a1 = seq.feed(b"trust and ");
        assert!(matches!(a1, StartupAction::None));
        // Second chunk: completes the line with second keyword + newline.
        let a2 = seq.feed(b"Allow access\n");
        match a2 {
            StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
            _ => panic!("expected Write(b\"\\r\") after line completed"),
        }
    }

    // ── idle-gap timer tests ──────────────────────────────────────────────────

    #[test]
    fn idle_fallback_fires_after_400ms_of_silence() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;

        assert!(matches!(
            seq.feed_at(&vec![b'x'; IDLE_THRESHOLD_BYTES], start),
            StartupAction::None
        ));
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(IDLE_TIMEOUT_MS - 1)),
            StartupAction::None
        ));

        match seq.poll_timers_at(start + Duration::from_millis(IDLE_TIMEOUT_MS)) {
            StartupAction::Write(bytes) => assert_eq!(bytes, b"\r"),
            other => panic!("expected idle fallback at 400ms, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    #[test]
    fn idle_fallback_output_restarts_quiet_window() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;

        seq.feed_at(&vec![b'x'; IDLE_THRESHOLD_BYTES], start);
        seq.feed_at(b"still rendering", start + Duration::from_millis(350));

        // More than 400ms has elapsed overall, but only 150ms has been quiet.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(500)),
            StartupAction::None
        ));
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(750)),
            StartupAction::Write(bytes) if bytes == b"\r"
        ));
    }

    #[test]
    fn default_post_dismiss_output_restarts_one_second_quiet_window() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;

        seq.feed_at(b"trust Allow folder\n", start);
        seq.feed_at(b"TUI redraw", start + Duration::from_millis(800));

        // One second has elapsed since CR, but only 200ms since the redraw.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(1000)),
            StartupAction::None
        ));
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(1800)),
            StartupAction::Write(payload) if payload.starts_with(b"\x1b[200~")
        ));
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }

    /// After trust-dismiss, new PTY output resets the idle gap so the timer
    /// does not fire while the TUI is still redrawing.
    #[test]
    fn idle_gap_resets_on_new_output() {
        let gap_ms: u64 = 60;
        let mut seq = StartupSeq::with_idle_gap(b"prompt".to_vec(), gap_ms);

        // Trigger trust dismiss via keyword line.
        seq.feed(b"trust Allow folder\n");
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // Wait until just before the gap would expire, then feed new output.
        std::thread::sleep(Duration::from_millis(gap_ms - 15));
        seq.feed(b"TUI redraw output\n");

        // Polling immediately after the reset must return None — the idle gap
        // restarted from the last output, so < 1 ms has passed.
        let action = seq.poll_timers();
        assert!(
            matches!(action, StartupAction::None),
            "idle gap must not fire immediately after output reset"
        );

        // After a full gap of silence from the reset, injection must fire.
        std::thread::sleep(Duration::from_millis(gap_ms + 20));
        let action = seq.poll_timers();
        match action {
            StartupAction::Write(payload) => {
                assert!(
                    payload.starts_with(b"\x1b[200~"),
                    "expected bracketed-paste open after idle gap"
                );
            }
            _ => panic!("expected Write (prompt injection) after idle gap expired post-reset"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }

    /// After trust-dismiss with no further PTY output, the idle gap fires and
    /// the prompt is injected via bracketed paste.
    #[test]
    fn idle_gap_fires_after_silence() {
        let gap_ms: u64 = 20;
        let mut seq = StartupSeq::with_idle_gap(b"hello world".to_vec(), gap_ms);

        // Trigger trust dismiss.
        seq.feed(b"trust Allow folder\n");
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // Polling before the gap expires must return None.
        let action = seq.poll_timers();
        assert!(
            matches!(action, StartupAction::None),
            "should not fire before idle gap elapses"
        );

        // Wait for silence.
        std::thread::sleep(Duration::from_millis(gap_ms + 10));

        let action = seq.poll_timers();
        match action {
            StartupAction::Write(payload) => {
                assert!(
                    payload.starts_with(b"\x1b[200~"),
                    "bracketed-paste open missing"
                );
                assert!(
                    payload.ends_with(b"\x1b[201~\r"),
                    "bracketed-paste close+CR missing"
                );
                assert!(
                    payload.windows(11).any(|w| w == b"hello world"),
                    "prompt text not in payload"
                );
            }
            _ => panic!("expected Write after idle gap expired"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }

    /// Idle-gap timer in TrustDismissed does not fire a second time after
    /// PromptInjected is reached.
    #[test]
    fn idle_gap_does_not_fire_after_prompt_injected() {
        let gap_ms: u64 = 10;
        let mut seq = StartupSeq::with_idle_gap(b"p".to_vec(), gap_ms);

        seq.feed(b"trust Allow folder\n");
        std::thread::sleep(Duration::from_millis(gap_ms + 10));

        // First poll → inject.
        let a1 = seq.poll_timers();
        assert!(matches!(a1, StartupAction::Write(_)));

        // Subsequent polls must be None.
        let a2 = seq.poll_timers();
        assert!(matches!(a2, StartupAction::None));
    }

    // ── prompt injection payload ──────────────────────────────────────────────

    #[test]
    fn make_prompt_payload_wraps_in_bracketed_paste() {
        let mut seq = StartupSeq::new(b"What is 2+2?".to_vec());
        // Force into TrustDismissed so we can call make_prompt_payload.
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();
        assert!(
            payload.starts_with(b"\x1b[200~"),
            "missing bracketed-paste open"
        );
        assert!(
            payload.ends_with(b"\x1b[201~\r"),
            "missing bracketed-paste close + CR"
        );
        assert!(
            payload.windows(12).any(|w| w == b"What is 2+2?"),
            "prompt text not present in payload"
        );
    }

    // ── prompt content delivery (bf-4rxh) ─────────────────────────────────────
    //
    // An earlier revision emitted `$(< <tmpfile>)` (a shell command-substitution)
    // for prompts above 32 KB. Bracketed paste delivers its payload to the Ink
    // REPL verbatim — there is no shell on the paste path to evaluate it — so the
    // model received the literal string `$(< /tmp/…)` instead of the prompt. The
    // payload must now carry the prompt bytes themselves at every size.

    /// A prompt larger than the former 32 KB inline threshold is delivered as its
    /// own *contents* inside the bracketed-paste envelope — never as a shell
    /// `$(< path)` substitution. (bf-4rxh acceptance criterion.)
    #[test]
    fn large_prompt_payload_carries_content_not_shell_substitution() {
        let body: Vec<u8> = b"X".repeat(32 * 1024 + 1);
        let mut seq = StartupSeq::new(body.clone());
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();

        // Must not embed the broken shell-substitution expression.
        assert!(
            !payload.windows(4).any(|w| w == b"$(< "),
            "payload must not embed a shell read command"
        );
        // Wrapped in a single bracketed-paste envelope + CR.
        assert!(
            payload.starts_with(b"\x1b[200~"),
            "missing bracketed-paste open"
        );
        assert!(
            payload.ends_with(b"\x1b[201~\r"),
            "missing bracketed-paste close + CR"
        );
        // Must carry the actual prompt bytes, verbatim and contiguous.
        assert!(
            payload.windows(body.len()).any(|w| w == body.as_slice()),
            "payload must contain the prompt content verbatim"
        );
    }

    /// Content (not shell substitution) is delivered at and around the former
    /// 32 KB boundary — the inline/relay split no longer exists.
    #[test]
    fn payload_carries_content_across_former_threshold() {
        for &n in &[32 * 1024 - 1, 32 * 1024, 32 * 1024 + 1] {
            let body: Vec<u8> = vec![b'Q'; n];
            let mut seq = StartupSeq::new(body.clone());
            seq.phase = StartupPhase::TrustDismissed;
            let payload = seq.make_prompt_payload();
            assert!(
                !payload.windows(4).any(|w| w == b"$(< "),
                "n={n}: payload must not embed a shell read command"
            );
            assert!(
                payload.windows(n).any(|w| w == body.as_slice()),
                "n={n}: payload must contain the prompt content verbatim"
            );
        }
    }

    /// Large-prompt delivery integrates through the full state machine:
    /// trust dismiss → idle gap → content payload injected.
    #[test]
    fn large_prompt_end_to_end_state_machine() {
        let gap_ms: u64 = 15;
        let body: Vec<u8> = b"C".repeat(32 * 1024 + 1);
        let mut seq = StartupSeq::with_idle_gap(body.clone(), gap_ms);

        seq.feed(b"trust Allow folder\n");
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        std::thread::sleep(Duration::from_millis(gap_ms + 10));

        let action = seq.poll_timers();
        match action {
            StartupAction::Write(payload) => {
                assert!(
                    payload.starts_with(b"\x1b[200~"),
                    "missing bracketed-paste open"
                );
                assert!(
                    payload.ends_with(b"\x1b[201~\r"),
                    "missing bracketed-paste close + CR"
                );
                assert!(
                    !payload.windows(4).any(|w| w == b"$(< "),
                    "large prompt must not inject a shell read command"
                );
                assert!(
                    payload.windows(body.len()).any(|w| w == body.as_slice()),
                    "large prompt payload must contain the content verbatim"
                );
            }
            _ => panic!("expected Write action from poll_timers for large prompt"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }
}
