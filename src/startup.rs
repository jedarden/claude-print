use std::time::{Duration, Instant};

// Trust dialog keyword set — 2+ on a single line → send CR.
const TRUST_KEYWORDS: &[&str] = &["trust", "Allow", "continue", "folder", "permission", "proceed"];
const KEYWORD_THRESHOLD: usize = 2;

const IDLE_THRESHOLD_BYTES: usize = 200;
const IDLE_TIMEOUT_MS: u64 = 800;
const HARD_TIMEOUT_SECS: u64 = 45;
// Time to wait after the dismiss CR before injecting the prompt.
const POST_DISMISS_IDLE_MS: u64 = 2000;
// Prompts larger than this are out-of-scope for inline injection (future: /read path).
const INLINE_PROMPT_MAX: usize = 32 * 1024;

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
    /// CR was sent to dismiss trust dialog; waiting for the 2 s idle before injection.
    TrustDismissed,
    /// Bracketed paste was sent; waiting for the Stop hook.
    PromptInjected,
}

/// Manages the startup handshake with the Claude Code TUI.
///
/// Phase 1: scan PTY output for trust-dialog keywords; send `\r` to dismiss.
/// Phase 2: after a 2 s quiet period, inject the user prompt via bracketed paste.
///
/// Call [`feed`] for every PTY chunk and [`poll_timers`] on each poll() iteration.
pub struct StartupSeq {
    phase: StartupPhase,
    prompt: Vec<u8>,
    bytes_received: usize,
    last_output_at: Instant,
    phase_start: Instant,
    trust_dismiss_at: Option<Instant>,
    /// Accumulates bytes from the current partial line for keyword scanning.
    line_buf: Vec<u8>,
}

impl StartupSeq {
    pub fn new(prompt: Vec<u8>) -> Self {
        let now = Instant::now();
        Self {
            phase: StartupPhase::Waiting,
            prompt,
            bytes_received: 0,
            last_output_at: now,
            phase_start: now,
            trust_dismiss_at: None,
            line_buf: Vec::new(),
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
        let count = TRUST_KEYWORDS
            .iter()
            .filter(|&&k| text.contains(k))
            .count();
        count >= KEYWORD_THRESHOLD
    }

    /// Feed a chunk of PTY output.
    ///
    /// Scans for trust keywords line-by-line.  Returns [`StartupAction::Write`]
    /// containing `b"\r"` on the first line that matches; no-ops in all other phases.
    /// Call [`poll_timers`] separately to handle deadline-driven transitions.
    pub fn feed(&mut self, chunk: &[u8]) -> StartupAction {
        let now = Instant::now();
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
    /// - Idle fallback (WAITING, ≥ 200 bytes, 0.8 s idle) → CR write
    /// - Post-dismiss idle (TRUST_DISMISSED, 2 s elapsed) → bracketed paste
    pub fn poll_timers(&mut self) -> StartupAction {
        let now = Instant::now();

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
                    self.phase = StartupPhase::TrustDismissed;
                    self.trust_dismiss_at = Some(now);
                    return StartupAction::Write(b"\r".to_vec());
                }

                StartupAction::None
            }

            StartupPhase::TrustDismissed => {
                if let Some(dismiss_at) = self.trust_dismiss_at {
                    if now.duration_since(dismiss_at)
                        >= Duration::from_millis(POST_DISMISS_IDLE_MS)
                    {
                        let payload = self.make_prompt_payload();
                        self.phase = StartupPhase::PromptInjected;
                        return StartupAction::Write(payload);
                    }
                }
                StartupAction::None
            }

            StartupPhase::PromptInjected => StartupAction::None,
        }
    }

    fn make_prompt_payload(&self) -> Vec<u8> {
        // Prompts > 32 KB would use /read <path>; inline path covers all normal cases.
        debug_assert!(
            self.prompt.len() <= INLINE_PROMPT_MAX,
            "large-prompt /read path not yet implemented"
        );
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

    // ── prompt injection payload ──────────────────────────────────────────────

    #[test]
    fn make_prompt_payload_wraps_in_bracketed_paste() {
        let mut seq = StartupSeq::new(b"What is 2+2?".to_vec());
        // Force into TrustDismissed so we can call make_prompt_payload.
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();
        assert!(payload.starts_with(b"\x1b[200~"), "missing bracketed-paste open");
        assert!(payload.ends_with(b"\x1b[201~\r"), "missing bracketed-paste close + CR");
        assert!(
            payload.windows(12).any(|w| w == b"What is 2+2?"),
            "prompt text not present in payload"
        );
    }
}
