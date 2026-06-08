use std::io::Write as IoWrite;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

// Trust dialog keyword set — 2+ on a single line → send CR.
const TRUST_KEYWORDS: &[&str] = &["trust", "Allow", "continue", "folder", "permission", "proceed"];
const KEYWORD_THRESHOLD: usize = 2;

const IDLE_THRESHOLD_BYTES: usize = 200;
const IDLE_TIMEOUT_MS: u64 = 800;
const HARD_TIMEOUT_SECS: u64 = 45;
/// Default idle-gap: ms of silence after trust-dismiss before injecting prompt.
/// Resets to zero on every PTY output chunk; fires only after uninterrupted silence.
pub const DEFAULT_POST_DISMISS_IDLE_MS: u64 = 2000;
/// Prompts at or below this size are injected inline via bracketed paste.
/// Larger prompts are written to a temp file and a shell `read` command is
/// injected instead, avoiding PTY pipe-buffer saturation.
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
    /// Temp file holding the prompt for the file-relay path (prompt > 32 KB).
    /// Kept alive here so the file persists until the session reads it.
    relay_file: Option<NamedTempFile>,
}

impl StartupSeq {
    pub fn new(prompt: Vec<u8>) -> Self {
        Self::with_idle_gap(prompt, DEFAULT_POST_DISMISS_IDLE_MS)
    }

    /// Construct with a custom post-dismiss idle gap in milliseconds.
    ///
    /// Primarily used in tests to avoid sleeping for 2 s.
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
            relay_file: None,
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
    /// - Post-dismiss idle gap (TRUST_DISMISSED, no output for `idle_gap_ms`) → bracketed paste
    ///
    /// The post-dismiss idle gap resets on every PTY chunk received via [`feed`].
    /// Injection fires only after `idle_gap_ms` ms of uninterrupted silence.
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
                    // Reset last_output_at so the idle gap is measured from the
                    // dismiss moment, not from whenever output last arrived in
                    // the Waiting phase.
                    self.last_output_at = now;
                    self.phase = StartupPhase::TrustDismissed;
                    self.trust_dismiss_at = Some(now);
                    return StartupAction::Write(b"\r".to_vec());
                }

                StartupAction::None
            }

            StartupPhase::TrustDismissed => {
                // Idle-gap check: fire only after `idle_gap_ms` ms of silence.
                // `last_output_at` is updated by feed() on every PTY chunk, so
                // any new output resets this window automatically.
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

    fn make_prompt_payload(&mut self) -> Vec<u8> {
        if self.prompt.len() <= INLINE_PROMPT_MAX {
            let mut out = Vec::with_capacity(self.prompt.len() + 12);
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(&self.prompt);
            out.extend_from_slice(b"\x1b[201~\r");
            out
        } else {
            self.make_file_relay_payload()
        }
    }

    /// Write the prompt to a temp file and return a bracketed-paste payload
    /// containing a shell `read` command (`$(< path)`) that substitutes the
    /// file contents.  Avoids saturating the PTY pipe buffer for large prompts.
    fn make_file_relay_payload(&mut self) -> Vec<u8> {
        let mut f = NamedTempFile::new().expect("create temp file for large prompt");
        f.write_all(&self.prompt)
            .expect("write large prompt to temp file");
        let path = f.path().to_owned();
        self.relay_file = Some(f);
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(b"$(< ");
        out.extend_from_slice(path.to_string_lossy().as_bytes());
        out.push(b')');
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
                assert!(payload.starts_with(b"\x1b[200~"), "bracketed-paste open missing");
                assert!(payload.ends_with(b"\x1b[201~\r"), "bracketed-paste close+CR missing");
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
        assert!(payload.starts_with(b"\x1b[200~"), "missing bracketed-paste open");
        assert!(payload.ends_with(b"\x1b[201~\r"), "missing bracketed-paste close + CR");
        assert!(
            payload.windows(12).any(|w| w == b"What is 2+2?"),
            "prompt text not present in payload"
        );
    }

    // ── large-prompt file relay ───────────────────────────────────────────────

    /// Prompts at or below INLINE_PROMPT_MAX use the inline bracketed-paste path.
    #[test]
    fn inline_path_used_at_threshold() {
        let prompt: Vec<u8> = b"Z".repeat(INLINE_PROMPT_MAX);
        let mut seq = StartupSeq::new(prompt.clone());
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();
        // Inline: open marker directly followed by prompt content.
        assert!(payload.starts_with(b"\x1b[200~"), "must start with bracketed-paste open");
        assert_eq!(payload[6], b'Z', "prompt byte must follow open marker immediately");
        // Must not contain shell substitution syntax.
        assert!(
            !payload.windows(4).any(|w| w == b"$(< "),
            "inline path must not emit shell read command"
        );
    }

    /// Prompts above INLINE_PROMPT_MAX write to a temp file and inject a shell
    /// `$(< path)` substitution via bracketed paste.
    #[test]
    fn file_relay_used_above_threshold() {
        let large_prompt: Vec<u8> = b"A".repeat(INLINE_PROMPT_MAX + 1);
        let mut seq = StartupSeq::new(large_prompt.clone());
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();
        // Must start with the shell substitution inside bracketed paste.
        assert!(
            payload.starts_with(b"\x1b[200~$(< "),
            "large prompt must inject shell read command"
        );
        assert!(
            payload.ends_with(b"\x1b[201~\r"),
            "must end with bracketed-paste close + CR"
        );
        // The relay_file field must be set (keeping the temp file alive).
        assert!(seq.relay_file.is_some(), "relay_file must be populated");
    }

    /// The temp file created by the file-relay path contains the exact prompt bytes.
    #[test]
    fn file_relay_temp_file_contains_prompt() {
        let large_prompt: Vec<u8> = b"B".repeat(INLINE_PROMPT_MAX + 256);
        let mut seq = StartupSeq::new(large_prompt.clone());
        seq.phase = StartupPhase::TrustDismissed;
        let payload = seq.make_prompt_payload();

        // Extract the path from payload: \x1b[200~$(< <path>)\x1b[201~\r
        let prefix = b"\x1b[200~$(< ";
        assert!(payload.starts_with(prefix));
        let after_prefix = &payload[prefix.len()..];
        let close_paren = after_prefix
            .iter()
            .position(|&b| b == b')')
            .expect("closing paren in payload");
        let path_bytes = &after_prefix[..close_paren];
        let path_str = std::str::from_utf8(path_bytes).expect("path is valid UTF-8");

        let file_content = std::fs::read(path_str).expect("temp file must exist while seq is alive");
        assert_eq!(
            file_content, large_prompt,
            "temp file must contain the full prompt"
        );
    }

    /// File-relay path integrates end-to-end through the state machine:
    /// trust dismiss → idle gap → file-relay payload injected.
    #[test]
    fn file_relay_end_to_end_state_machine() {
        let gap_ms: u64 = 15;
        let large_prompt: Vec<u8> = b"C".repeat(INLINE_PROMPT_MAX + 1);
        let mut seq = StartupSeq::with_idle_gap(large_prompt.clone(), gap_ms);

        seq.feed(b"trust Allow folder\n");
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        std::thread::sleep(Duration::from_millis(gap_ms + 10));

        let action = seq.poll_timers();
        match action {
            StartupAction::Write(payload) => {
                assert!(
                    payload.starts_with(b"\x1b[200~$(< "),
                    "large prompt must use file-relay injection"
                );
            }
            _ => panic!("expected Write action from poll_timers for large prompt"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }
}
