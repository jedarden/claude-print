use std::time::{Duration, Instant};

// Trust dialog keyword set — 2+ on a single line → the trust dialog is on screen.
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
/// Quiet period required before the no-dialog idle fallback dismisses unknown output.
const IDLE_TIMEOUT_MS: u64 = 400;
const HARD_TIMEOUT_SECS: u64 = 45;
/// Default idle-gap: ms of silence after trust-dismiss before injecting prompt.
/// Resets to zero on every PTY output chunk; fires only after uninterrupted silence.
pub const DEFAULT_POST_DISMISS_IDLE_MS: u64 = 1000;

/// Raw Waiting-phase output retained for dialog analysis.  The trust dialog
/// renders within the first kilobytes; the cap only bounds a pathologically
/// chatty TUI so the capture can never grow without limit.
const CAPTURE_MAX_BYTES: usize = 64 * 1024;
/// Silence after which a detected-but-unidentified dialog is treated as settled:
/// the TUI has stopped rendering, so the trusting entry is never going to appear.
const DIALOG_SETTLE_MS: u64 = 2_000;
/// Quiet period required after the trusting entry has been identified before the
/// dismissal keys may be sent. Keys written while the TUI is still painting are
/// silently dropped: claude's Ink shell attaches its stdin handler only after
/// the initial render burst, so keys sent at the first parseable instant leave
/// the dialog up — and the prompt payload's trailing CR then confirms the
/// highlighted "No, exit" (claudepr-fe3d3160). Probed against real claude
/// 2.1.269 under a PTY (2026-09-12): keys sent with 0 ms of quiet after the
/// render burst never register trust; ≥ 150 ms of quiet always do. 400 ms
/// (matching [`IDLE_TIMEOUT_MS`]) is ~2.5× that floor for slower machines.
const DISMISS_SETTLE_MS: u64 = 400;
/// Absolute ceiling on the Waiting phase once a dialog is detected — both for
/// waiting for a positively-identified trusting entry and for waiting for the
/// quiet a positively-identified dialog needs before its keys may be sent.
/// Guards against a TUI that never stops repainting.
const DIALOG_IDENTIFY_TIMEOUT_MS: u64 = 15_000;

/// Caret the Claude Code TUI renders to the left of the highlighted entry.
const CARET_MARK: &str = "\u{276f}";
/// ASCII fallback caret, honoured only at the start of a line.
const CARET_MARK_ASCII: &str = "> ";
const DOWN_ARROW: &[u8] = b"\x1b[B";
const UP_ARROW: &[u8] = b"\x1b[A";

/// Action requested by [`StartupSeq`] from the event loop.
#[derive(Debug)]
pub enum StartupAction {
    /// Write these bytes to the PTY master fd.
    Write(Vec<u8>),
    /// No action needed this iteration.
    None,
    /// Hard timeout fired (≤ 200 bytes in 45 s) — caller should SIGTERM child and exit 2.
    HardTimeout,
    /// The trust dialog was detected but the trusting entry could not be
    /// positively identified, so the dismissal was refused rather than
    /// confirming a highlighted entry that may be "No, exit" (claudepr-fe3d3160).
    /// The payload is a diagnostic message; the caller should kill the child and
    /// fail loudly with it.
    Refuse(String),
}

/// Phase of the startup sequence.
#[derive(Debug, Clone, PartialEq)]
pub enum StartupPhase {
    /// Waiting for the trust dialog to be positively dismissable or for the
    /// no-dialog idle fallback.
    Waiting,
    /// Trust keys were sent to dismiss trust dialog; waiting for quiet before injection.
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

/// Which way a rendered dialog entry points the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    /// The entry that grants trust ("Yes, I trust this folder", "Yes, proceed", …).
    Trusting,
    /// The entry that refuses it ("No, exit", "No, cancel", …).
    Refusing,
}

/// One rendered line that occupies a slot in a dialog's selection order.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogEntry {
    /// The line carried the caret marker — this entry is the highlighted one.
    pub highlighted: bool,
    /// `Some` when the entry text classifies as trusting/refusing.
    pub kind: Option<OptionKind>,
    /// Escape-stripped entry text, for diagnostics.
    pub text: String,
}

/// Result of analysing the captured Waiting-phase output.
#[derive(Debug, Clone, Default)]
pub struct DialogScan {
    /// A dialog requiring an explicit selection is (probably) on screen.
    pub dialog_present: bool,
    /// Selectable entries in rendered order.
    pub entries: Vec<DialogEntry>,
    /// Last few non-blank rendered lines, for failure diagnostics.
    pub tail: String,
}

impl DialogScan {
    /// Index into [`Self::entries`] of the highlighted entry, if the caret is visible.
    ///
    /// The *last* caret marker wins: an Ink TUI redraws its lines in place, so
    /// the raw capture can hold several copies of the dialog and only the most
    /// recent render describes the current selection.
    pub fn caret_index(&self) -> Option<usize> {
        self.entries.iter().rposition(|e| e.highlighted)
    }

    /// Index into [`Self::entries`] of the trusting entry, if positively identified.
    pub fn trusting_index(&self) -> Option<usize> {
        self.entries
            .iter()
            .rposition(|e| e.kind == Some(OptionKind::Trusting))
    }
}

/// What must be sent to positively select the trusting entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DismissPlan {
    /// The trusting entry is already highlighted — Enter confirms it.
    Confirm,
    /// Move the caret this many entries (positive = down, negative = up) first.
    Move(isize),
}

/// Manages the startup handshake with the Claude Code TUI.
///
/// Phase 1: capture PTY output and watch it for the trust dialog.  The dialog is
/// dismissed by positively selecting the trusting entry — the caret is matched
/// against the rendered entry text and moved onto "Yes, …" with arrow keys
/// before Enter (claude 2.1.263 highlights "No, exit" by default, so confirming
/// whatever is highlighted refuses the dialog and kills the session).  The keys
/// are held until the TUI has been quiet for [`DISMISS_SETTLE_MS`]: keys written
/// while the TUI is still painting its first frame are silently dropped, which
/// leaves the dialog up for the prompt's trailing CR to answer with the
/// highlighted "No, exit".  When the trusting entry cannot be positively
/// identified the dismissal is *refused* with [`StartupAction::Refuse`] instead
/// of guessing.  A startup screen with no dialog at all (a trusted cwd drops
/// straight into the REPL) still takes the original idle fallback: ≥ 200 bytes
/// then 0.4 s quiet → a harmless bare Enter.
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
    /// entering TrustDismissed.  Used as the start of the idle-gap window.
    last_output_at: Instant,
    phase_start: Instant,
    trust_dismiss_at: Option<Instant>,
    /// First instant a trust dialog was seen in the capture, for the
    /// identify deadline.
    dialog_detected_at: Option<Instant>,
    /// Raw Waiting-phase output, capped at [`CAPTURE_MAX_BYTES`].
    capture: Vec<u8>,
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
            dialog_detected_at: None,
            capture: Vec::new(),
            idle_gap_ms,
        }
    }

    /// Construct for a **prewarmed** worker (ADR-005 pool path).
    ///
    /// The daemon's own warmup (`warm_worker_to_outcome`) has already driven the
    /// child through trust-dismiss and the post-dismiss settle — it reuses
    /// `WARMUP_SETTLE_MS`, which is [`DEFAULT_POST_DISMISS_IDLE_MS`] by
    /// construction, so both paths agree on what "settled" means. This
    /// sequencer therefore starts at [`StartupPhase::TrustDismissed`] and owes
    /// only the quiet-window wait before injecting the prompt.
    ///
    /// Starting at `Waiting` would be wrong twice over on an idle prewarmed
    /// REPL: its startup output stream finished during warmup, so fresh bytes
    /// never reach [`IDLE_THRESHOLD_BYTES`] and the 45 s hard timeout would
    /// eventually fire — and the Waiting-phase idle fallback would send a bare
    /// CR at an already-idle prompt, submitting an empty turn.
    ///
    /// `last_output_at` is stamped `now` (and `trust_dismiss_at`, for the
    /// trace, records the handoff instant): the caller still gets a full
    /// `idle_gap_ms` of observed silence before any bytes are written to the
    /// worker, re-establishing client-side the same settled-before-injection
    /// guarantee the daemon's warmup provided.
    pub fn prewarmed(prompt: Vec<u8>) -> Self {
        Self::prewarmed_with_idle_gap(prompt, DEFAULT_POST_DISMISS_IDLE_MS)
    }

    /// [`Self::prewarmed`] with a custom injection quiet window (ms).
    pub fn prewarmed_with_idle_gap(prompt: Vec<u8>, idle_gap_ms: u64) -> Self {
        let now = Instant::now();
        Self {
            phase: StartupPhase::TrustDismissed,
            prompt,
            bytes_received: 0,
            last_output_at: now,
            phase_start: now,
            trust_dismiss_at: Some(now),
            dialog_detected_at: None,
            capture: Vec::new(),
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

    /// Pure decision helper: the key sequence that positively selects the
    /// trusting entry of the dialog rendered in `capture`, or `None` when the
    /// trusting entry (or the caret) cannot be positively identified.
    pub fn plan_keys(capture: &[u8]) -> Option<Vec<u8>> {
        Self::plan_dismiss(&Self::analyze(capture)).map(|p| Self::dismiss_payload(&p))
    }

    /// Pure detection helper: is a selection dialog visible in `capture`?
    pub fn dialog_present(capture: &[u8]) -> bool {
        Self::analyze(capture).dialog_present
    }

    /// Feed a chunk of PTY output.
    ///
    /// Scans the accumulated output for the trust dialog on every chunk and
    /// returns the dismissal key sequence as soon as the trusting entry is
    /// positively identified.  Call [`poll_timers`] separately to handle
    /// deadline-driven transitions.
    pub fn feed(&mut self, chunk: &[u8]) -> StartupAction {
        self.feed_at(chunk, Instant::now())
    }

    fn feed_at(&mut self, chunk: &[u8], now: Instant) -> StartupAction {
        self.bytes_received += chunk.len();
        self.last_output_at = now;

        if self.phase != StartupPhase::Waiting {
            return StartupAction::None;
        }

        self.capture.extend_from_slice(chunk);
        if self.capture.len() > CAPTURE_MAX_BYTES {
            let excess = self.capture.len() - CAPTURE_MAX_BYTES;
            self.capture.drain(..excess);
        }

        self.waiting_step(now)
    }

    /// Poll deadline-driven transitions.  Call once per poll() iteration.
    ///
    /// Handles:
    /// - Hard timeout (WAITING, < 200 bytes in 45 s, no dialog on screen) → [`StartupAction::HardTimeout`]
    /// - Idle fallback (WAITING, no dialog, ≥ 200 bytes, 0.4 s quiet) → bare CR
    /// - Unidentified dialog (WAITING, dialog present, screen settled) → [`StartupAction::Refuse`]
    /// - Post-dismiss idle gap (TRUST_DISMISSED, no output for `idle_gap_ms`) → bracketed paste
    ///
    /// The post-dismiss transitions require uninterrupted silence. Every PTY
    /// chunk received via [`feed`] restarts the current quiet window, so a
    /// settled TUI advances after 0.4 s / 1.0 s while an actively rendering TUI
    /// continues to wait.
    pub fn poll_timers(&mut self) -> StartupAction {
        self.poll_timers_at(Instant::now())
    }

    fn poll_timers_at(&mut self, now: Instant) -> StartupAction {
        match self.phase {
            StartupPhase::Waiting => self.waiting_step(now),
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

    /// One Waiting-phase decision step, driven by both [`Self::feed_at`] (new
    /// output arrived) and [`Self::poll_timers_at`] (a timer tick fired).
    fn waiting_step(&mut self, now: Instant) -> StartupAction {
        let scan = Self::analyze(&self.capture);

        if scan.dialog_present {
            if self.dialog_detected_at.is_none() {
                self.dialog_detected_at = Some(now);
            }
            // claudepr-fe3d3160: never confirm a highlighted entry we have not
            // positively determined to be the trusting one — claude 2.1.263
            // highlights "No, exit" by default, so a blind Enter kills the
            // session in every untrusted cwd.
            if let Some(plan) = Self::plan_dismiss(&scan) {
                let detected_at = self.dialog_detected_at.unwrap_or(now);
                let quiet = now.duration_since(self.last_output_at);
                if quiet >= Duration::from_millis(DISMISS_SETTLE_MS) {
                    // The TUI has gone quiet since its last paint, so its input
                    // handler is attached and the keys will actually be read.
                    let keys = Self::dismiss_payload(&plan);
                    self.last_output_at = now;
                    self.phase = StartupPhase::TrustDismissed;
                    self.trust_dismiss_at = Some(now);
                    return StartupAction::Write(keys);
                }
                let expired = now.duration_since(detected_at)
                    >= Duration::from_millis(DIALOG_IDENTIFY_TIMEOUT_MS);
                if expired {
                    // The trusting entry is on screen but the TUI has been
                    // repainting for the whole ceiling without ever going quiet
                    // — keys sent now may be dropped the same way, and the
                    // prompt's trailing CR would then confirm "No, exit".
                    // Refusing is strictly safer than guessing (claudepr-fe3d3160).
                    return StartupAction::Refuse(Self::refusal_reason(&scan));
                }
                // Identified, but the TUI is still painting — hold the keys
                // until it has been quiet for DISMISS_SETTLE_MS.
                return StartupAction::None;
            }

            let detected_at = self.dialog_detected_at.unwrap_or(now);
            let settled =
                now.duration_since(self.last_output_at) >= Duration::from_millis(DIALOG_SETTLE_MS);
            let expired = now.duration_since(detected_at)
                >= Duration::from_millis(DIALOG_IDENTIFY_TIMEOUT_MS);
            if settled || expired {
                return StartupAction::Refuse(Self::refusal_reason(&scan));
            }
            // The dialog is mid-render — the trusting entry may still arrive.
            return StartupAction::None;
        }

        // No dialog on screen: the original heuristic dismissal paths.
        if now.duration_since(self.phase_start) >= Duration::from_secs(HARD_TIMEOUT_SECS)
            && self.bytes_received < IDLE_THRESHOLD_BYTES
        {
            return StartupAction::HardTimeout;
        }

        if self.bytes_received >= IDLE_THRESHOLD_BYTES
            && now.duration_since(self.last_output_at) >= Duration::from_millis(IDLE_TIMEOUT_MS)
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

    /// Analyse the Waiting-phase capture: strip escape sequences, split the
    /// rendered lines, and classify the dialog entries.
    fn analyze(capture: &[u8]) -> DialogScan {
        let plain = strip_escapes(capture);
        let mut scan = DialogScan::default();
        let mut keyword_lines = 0usize;
        let mut confirm_footer = false;
        let mut tail_lines: Vec<String> = Vec::new();

        for raw_line in plain.split(|&b| b == b'\n' || b == b'\r') {
            let line = String::from_utf8_lossy(raw_line);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            tail_lines.push(trimmed.to_owned());
            if Self::scan_line(raw_line) {
                keyword_lines += 1;
            }
            let lower = trimmed.to_lowercase();
            if lower.contains("enter to confirm") || lower.contains("esc to cancel") {
                confirm_footer = true;
            }

            let (highlighted, body) = split_caret(trimmed);
            let kind = classify(&body);
            if !highlighted && kind.is_none() {
                // Ordinary rendered text — not a dialog entry.
                continue;
            }
            scan.entries.push(DialogEntry {
                highlighted,
                kind,
                text: body,
            });
        }

        let has_pair = scan
            .entries
            .iter()
            .any(|e| e.kind == Some(OptionKind::Trusting))
            && scan
                .entries
                .iter()
                .any(|e| e.kind == Some(OptionKind::Refusing));
        let caret_on_classified = scan
            .entries
            .iter()
            .any(|e| e.highlighted && e.kind.is_some());
        scan.dialog_present =
            keyword_lines > 0 || has_pair || confirm_footer || caret_on_classified;

        // Diagnostic tail: the last few rendered lines, bounded.
        let start = tail_lines.len().saturating_sub(5);
        let joined = tail_lines[start..].join(" | ");
        scan.tail = if joined.len() > 240 {
            let mut cut = 240;
            while !joined.is_char_boundary(cut) {
                cut -= 1;
            }
            format!("{}…", &joined[..cut])
        } else {
            joined
        };

        scan
    }

    /// Decide how to dismiss a scanned dialog, or `None` when the trusting
    /// entry cannot be positively identified.
    fn plan_dismiss(scan: &DialogScan) -> Option<DismissPlan> {
        let caret = scan.caret_index()?;
        let trusting = scan.trusting_index()?;
        Some(if caret == trusting {
            DismissPlan::Confirm
        } else {
            DismissPlan::Move(trusting as isize - caret as isize)
        })
    }

    /// Build the key sequence for a dismissal plan: arrow keys to move the caret
    /// onto the trusting entry (when it is not already there), then Enter.
    fn dismiss_payload(plan: &DismissPlan) -> Vec<u8> {
        let mut out = Vec::new();
        if let DismissPlan::Move(delta) = *plan {
            let (key, count) = if delta > 0 {
                (DOWN_ARROW, delta as usize)
            } else {
                (UP_ARROW, delta.unsigned_abs())
            };
            for _ in 0..count {
                out.extend_from_slice(key);
            }
        }
        out.push(b'\r');
        out
    }

    /// Diagnostic message for a refused dismissal: what the TUI actually
    /// rendered and what the operator can do about it.
    fn refusal_reason(scan: &DialogScan) -> String {
        let detail = if scan.trusting_index().is_none() {
            "no trusting entry (an option starting with \"Yes\") is visible in the startup output"
        } else {
            "the trusting entry is visible but no caret marker (❯) is, so the highlighted entry \
             cannot be determined"
        };
        format!(
            "trust dialog detected but the trusting entry could not be positively identified: \
             {detail}. Refusing to confirm a possibly-refusing default (claude 2.1.263 highlights \
             \"No, exit\" first, and confirming it kills the session). Re-run with --pretrust-cwd \
             to pre-grant trust for this directory, or accept the trust dialog once in an \
             interactive claude session. Startup output tail: {}",
            scan.tail
        )
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

/// Remove ANSI escape sequences and non-text control bytes, keeping newlines,
/// carriage returns, tabs, printable ASCII and raw UTF-8 bytes so the rendered
/// dialog (including the `❯` caret) can be matched as text.
fn strip_escapes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b != 0x1b {
            if b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7f).contains(&b) || b >= 0x80 {
                out.push(b);
            }
            i += 1;
            continue;
        }
        match input.get(i + 1) {
            // CSI: parameters then a final byte in 0x40..=0x7e.
            Some(b'[') => {
                let mut j = i + 2;
                while j < input.len() && !(0x40..=0x7e).contains(&input[j]) {
                    j += 1;
                }
                i = (j + 1).min(input.len());
            }
            // OSC: terminated by BEL or ST (ESC \).
            Some(b']') => {
                let mut j = i + 2;
                while j < input.len() {
                    if input[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if input[j] == 0x1b && input.get(j + 1) == Some(&b'\\') {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            // Two-byte escape (ESC 7, ESC (, ESC =, …).
            Some(_) => i += 2,
            None => i += 1,
        }
    }
    out
}

/// Split a rendered line into `(highlighted, entry_text)` by stripping a leading
/// caret marker.
fn split_caret(line: &str) -> (bool, String) {
    if let Some(rest) = line.strip_prefix(CARET_MARK) {
        return (true, rest.trim().to_string());
    }
    if let Some(rest) = line.strip_prefix(CARET_MARK_ASCII) {
        return (true, rest.trim().to_string());
    }
    (false, line.to_string())
}

/// Classify an entry body as the trusting or the refusing option.
///
/// Matching is on the entry's opening word so phrasing changes ("Yes, proceed",
/// "Yes, I trust this folder", "No, exit", "No, cancel") are all covered without
/// relying on ordering or on which entry the TUI highlights by default.
fn classify(body: &str) -> Option<OptionKind> {
    let lowered = body.to_lowercase();
    let body = strip_numbering(&lowered);
    let body = body.trim_start();
    if starts_with_word(body, "yes") {
        return Some(OptionKind::Trusting);
    }
    if starts_with_word(body, "no") {
        return Some(OptionKind::Refusing);
    }
    None
}

/// Drop a leading list number ("1.", "2)", "[3]") so numbered menus classify the
/// same way as plain ones.
fn strip_numbering(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut j = 0;
    if bytes.first() == Some(&b'[') {
        j += 1;
    }
    let digits_start = j;
    while bytes.get(j).is_some_and(|b| b.is_ascii_digit()) {
        j += 1;
    }
    if j == digits_start {
        return s;
    }
    if bytes.get(j) == Some(&b']') {
        j += 1;
    }
    while matches!(
        bytes.get(j),
        Some(b'.') | Some(b')') | Some(b':') | Some(b'-') | Some(b' ')
    ) {
        j += 1;
    }
    &s[j..]
}

/// `s` begins with `word` as a standalone word ("yes," / "no." / "no " / "no"),
/// not as a prefix of a longer one ("notes" is not "no").
fn starts_with_word(s: &str, word: &str) -> bool {
    let Some(rest) = s.strip_prefix(word) else {
        return false;
    };
    match rest.chars().next() {
        None => true,
        Some(c) => !c.is_ascii_alphanumeric(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trust dialog as claude 2.1.263 actually renders it in an untrusted
    /// cwd, screen-scraped from a real TUI under a PTY (claudepr-fe3d3160).
    /// The REFUSING entry is highlighted by default.
    const DIALOG_2_1_263: &str = concat!(
        "Quick safety check: Is this a project you created or one you trust?\r\n",
        "\u{276f} No, exit\r\n",
        "  Yes, I trust this folder\r\n",
        "Enter to confirm \u{b7} Esc to cancel\r\n",
    );

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

    // ── dialog analysis (pure helpers) ────────────────────────────────────────

    #[test]
    fn dialog_2_1_263_detected_and_down_arrow_required() {
        assert!(StartupSeq::dialog_present(DIALOG_2_1_263.as_bytes()));
        let keys = StartupSeq::plan_keys(DIALOG_2_1_263.as_bytes())
            .expect("the trusting entry must be positively identified");
        assert_eq!(
            keys, b"\x1b[B\r",
            "caret on 'No, exit' must move down, then Enter"
        );
    }

    #[test]
    fn caret_on_trusting_entry_confirms_in_place() {
        let dialog = concat!(
            "Do you trust the files in this folder?\r\n",
            "\u{276f} Yes, proceed\r\n",
            "  No, exit\r\n",
        );
        let keys = StartupSeq::plan_keys(dialog.as_bytes()).expect("trusting entry identified");
        assert_eq!(
            keys, b"\r",
            "caret already on the trusting entry — plain Enter"
        );
    }

    #[test]
    fn caret_below_trusting_entry_moves_up() {
        let dialog = concat!(
            "Do you trust the files in this folder?\r\n",
            "  Yes, proceed\r\n",
            "\u{276f} No, exit\r\n",
        );
        let keys = StartupSeq::plan_keys(dialog.as_bytes()).expect("trusting entry identified");
        assert_eq!(
            keys, b"\x1b[A\r",
            "trusting entry above the caret must move up"
        );
    }

    #[test]
    fn numbered_menu_entries_classify_the_same_way() {
        let dialog = concat!(
            "Do you trust the files in this folder?\r\n",
            "\u{276f} 1. No, exit\r\n",
            "  2) Yes, proceed\r\n",
        );
        let keys = StartupSeq::plan_keys(dialog.as_bytes()).expect("numbered trusting entry found");
        assert_eq!(keys, b"\x1b[B\r");
    }

    #[test]
    fn ansi_wrapped_dialog_still_parses() {
        // The real TUI colourises the caret and the highlighted entry.
        let dialog = concat!(
            "Quick safety check: Is this a project you created or one you trust?\r\n",
            "\x1b[1;36m\u{276f}\x1b[0m \x1b[36mNo, exit\x1b[0m\r\n",
            "  Yes, I trust this folder\r\n",
            "Enter to confirm \u{b7} Esc to cancel\r\n",
        );
        let keys = StartupSeq::plan_keys(dialog.as_bytes()).expect("colored dialog parses");
        assert_eq!(keys, b"\x1b[B\r");
    }

    #[test]
    fn dialog_redraw_uses_the_latest_caret_position() {
        // An Ink TUI repaints the same lines; the newest render wins.
        let mut capture = DIALOG_2_1_263.to_string();
        capture.push_str(concat!(
            "\x1b[2K  No, exit\r\n",
            "\x1b[2K\u{276f} Yes, I trust this folder\r\n",
        ));
        let keys = StartupSeq::plan_keys(capture.as_bytes()).expect("trusting entry identified");
        assert_eq!(
            keys, b"\r",
            "the latest render has the caret on the trusting entry"
        );
    }

    #[test]
    fn unrecognised_option_wording_is_not_confirmed() {
        // A future rewording with no Yes/No entries: detection still fires
        // (keywords + confirm footer) but nothing may be confirmed blindly.
        let dialog = concat!(
            "Quick safety check: trust this folder before you continue?\r\n",
            "\u{276f} Depart\r\n",
            "  Remain\r\n",
            "Enter to confirm \u{b7} Esc to cancel\r\n",
        );
        assert!(StartupSeq::dialog_present(dialog.as_bytes()));
        assert!(
            StartupSeq::plan_keys(dialog.as_bytes()).is_none(),
            "no trusting entry identified — must not produce keys"
        );
    }

    #[test]
    fn missing_caret_marker_is_not_confirmed() {
        let dialog = concat!(
            "Quick safety check: Is this a project you created or one you trust?\r\n",
            "  No, exit\r\n",
            "  Yes, I trust this folder\r\n",
            "Enter to confirm \u{b7} Esc to cancel\r\n",
        );
        assert!(StartupSeq::dialog_present(dialog.as_bytes()));
        assert!(
            StartupSeq::plan_keys(dialog.as_bytes()).is_none(),
            "without a visible caret the highlighted entry is unknown"
        );
    }

    #[test]
    fn repl_banner_is_not_a_dialog() {
        // A trusted cwd drops straight into the REPL: no Yes/No pair, no
        // keywords, no confirm footer — so the idle fallback still applies.
        let banner = concat!(
            "Welcome to Claude Code\r\n",
            "\r\n",
            " \u{276f} Ask claude anything\r\n",
            "\r\n",
            "? Try \"run tests in src/\"\r\n",
        );
        assert!(!StartupSeq::dialog_present(banner.as_bytes()));
    }

    // ── caret_index / trusting_index / plan derivation ────────────────────────

    #[test]
    fn caret_index_and_trusting_index_locate_the_entries() {
        let scan = StartupSeq::analyze(DIALOG_2_1_263.as_bytes());
        assert_eq!(
            scan.caret_index(),
            Some(0),
            "the caret renders on 'No, exit' by default in claude 2.1.263"
        );
        assert_eq!(
            scan.trusting_index(),
            Some(1),
            "'Yes, I trust this folder' is the trusting entry"
        );
    }

    #[test]
    fn caret_index_prefers_the_newest_render() {
        // A repaint appends two more entries; the caret entry of the latest
        // render wins over the stale one from the first paint.
        let mut capture = DIALOG_2_1_263.to_string();
        capture.push_str("\x1b[2K  No, exit\r\n\x1b[2K\u{276f} Yes, I trust this folder\r\n");
        let scan = StartupSeq::analyze(capture.as_bytes());
        assert_eq!(scan.caret_index(), Some(3));
        assert_eq!(scan.trusting_index(), Some(3));
    }

    #[test]
    fn caret_index_none_without_a_caret_marker() {
        let capture = "  No, exit\r\n  Yes, I trust this folder\r\n";
        let scan = StartupSeq::analyze(capture.as_bytes());
        assert_eq!(scan.caret_index(), None, "no caret rendered anywhere");
        assert_eq!(scan.trusting_index(), Some(1));
    }

    #[test]
    fn trusting_index_none_without_an_identifiable_trusting_entry() {
        let capture = "\u{276f} Depart\r\n  Remain\r\n";
        let scan = StartupSeq::analyze(capture.as_bytes());
        assert_eq!(scan.caret_index(), Some(0));
        assert_eq!(scan.trusting_index(), None);
        // A highlighted-but-unclassified entry (the REPL banner caret) does not
        // count as either.
        let banner_scan = StartupSeq::analyze(" \u{276f} Ask claude anything\r\n".as_bytes());
        assert_eq!(banner_scan.caret_index(), Some(0));
        assert_eq!(banner_scan.trusting_index(), None);
    }

    #[test]
    fn plan_confirms_when_caret_is_already_on_the_trusting_entry() {
        let scan = StartupSeq::analyze(
            concat!(
                "Do you trust the files in this folder?\r\n",
                "\u{276f} Yes, proceed\r\n",
                "  No, exit\r\n",
            )
            .as_bytes(),
        );
        assert_eq!(scan.caret_index(), scan.trusting_index());
        assert_eq!(
            StartupSeq::plan_dismiss(&scan),
            Some(DismissPlan::Confirm),
            "caret on the trusting entry — plain Enter, no arrows"
        );
    }

    #[test]
    fn plan_moves_down_to_the_trusting_entry() {
        let scan = StartupSeq::analyze(DIALOG_2_1_263.as_bytes());
        assert_eq!(StartupSeq::plan_dismiss(&scan), Some(DismissPlan::Move(1)));
    }

    #[test]
    fn plan_moves_up_when_the_trusting_entry_is_above_the_caret() {
        let scan = StartupSeq::analyze(
            concat!(
                "Do you trust the files in this folder?\r\n",
                "  Yes, proceed\r\n",
                "\u{276f} No, exit\r\n",
            )
            .as_bytes(),
        );
        assert_eq!(
            StartupSeq::plan_dismiss(&scan),
            Some(DismissPlan::Move(-1)),
            "trusting entry above the caret — one Up arrow"
        );
    }

    #[test]
    fn plan_is_none_when_the_trusting_entry_cannot_be_identified() {
        // Trusting entry visible but no caret: the highlighted entry is unknown.
        let scan = StartupSeq::analyze("  No, exit\r\n  Yes, I trust this folder\r\n".as_bytes());
        assert_eq!(StartupSeq::plan_dismiss(&scan), None);
        // Caret visible but no trusting entry: nothing safe to select.
        let scan = StartupSeq::analyze("\u{276f} Depart\r\n  Remain\r\n".as_bytes());
        assert_eq!(StartupSeq::plan_dismiss(&scan), None);
        // Empty capture: neither is known.
        assert_eq!(StartupSeq::plan_dismiss(&StartupSeq::analyze(b"")), None);
    }

    #[test]
    fn dismiss_payload_derivation_matches_the_plan() {
        assert_eq!(StartupSeq::dismiss_payload(&DismissPlan::Confirm), b"\r");
        assert_eq!(
            StartupSeq::dismiss_payload(&DismissPlan::Move(1)),
            b"\x1b[B\r"
        );
        assert_eq!(
            StartupSeq::dismiss_payload(&DismissPlan::Move(2)),
            b"\x1b[B\x1b[B\r"
        );
        assert_eq!(
            StartupSeq::dismiss_payload(&DismissPlan::Move(-1)),
            b"\x1b[A\r"
        );
        assert_eq!(
            StartupSeq::dismiss_payload(&DismissPlan::Move(-2)),
            b"\x1b[A\x1b[A\r"
        );
    }

    #[test]
    fn strip_escapes_removes_csi_and_osc_but_keeps_utf8() {
        let raw = "\x1b[2K\u{276f}\x1b]0;title\x07 No, exit\r\n";
        let plain = String::from_utf8_lossy(&strip_escapes(raw.as_bytes())).into_owned();
        assert_eq!(plain, "\u{276f} No, exit\r\n");
    }

    // ── feed() unit tests ─────────────────────────────────────────────────────

    #[test]
    fn feed_caret_on_refuse_dialog_sends_down_then_cr() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        // Question line arrives first: the dialog is detected but the options
        // are not on screen yet, so nothing may be sent.
        let a1 =
            seq.feed(b"Quick safety check: Is this a project you created or one you trust?\r\n");
        assert!(
            matches!(a1, StartupAction::None),
            "options not rendered yet"
        );
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
        // Options arrive with the caret on the refusing entry. The keys are
        // identified instantly but HELD — the TUI is still painting, and keys
        // written mid-paint are dropped (claudepr-fe3d3160).
        let options = "\u{276f} No, exit\r\n  Yes, I trust this folder\r\n";
        let a2 = seq.feed(options.as_bytes());
        assert!(
            matches!(a2, StartupAction::None),
            "keys must wait for the post-render quiet window, got {a2:?}"
        );
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
        // Once the TUI has been quiet for the settle window, the keys fire.
        match seq.poll_timers_at(Instant::now() + Duration::from_millis(DISMISS_SETTLE_MS)) {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    /// The settle guard itself (claudepr-fe3d3160 follow-up): a fully rendered
    /// dialog with the caret on "No, exit" must NOT produce keys at the
    /// parseable instant, and must not produce them before the quiet window
    /// completes — keys fired mid-paint are dropped and the prompt's trailing
    /// CR then confirms "No, exit".
    #[test]
    fn dismissal_keys_held_until_tui_goes_quiet() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let start = seq.phase_start;
        let a = seq.feed_at(DIALOG_2_1_263.as_bytes(), start);
        assert!(
            matches!(a, StartupAction::None),
            "no keys at the parseable instant — the TUI is still painting"
        );
        assert_eq!(*seq.phase(), StartupPhase::Waiting);

        // Just short of the quiet window: still holding.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(DISMISS_SETTLE_MS - 50)),
            StartupAction::None
        ));
        assert_eq!(*seq.phase(), StartupPhase::Waiting);

        // Quiet window complete: down-arrow, then Enter.
        match seq.poll_timers_at(start + Duration::from_millis(DISMISS_SETTLE_MS)) {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    /// A dialog identified but whose TUI never goes quiet must refuse at the
    /// identify ceiling rather than fire keys a rendering TUI would drop.
    #[test]
    fn identified_but_never_quiet_dialog_refuses_at_deadline() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let start = seq.phase_start;
        seq.feed_at(DIALOG_2_1_263.as_bytes(), start);
        // Repaint every 300 ms — quiet never reaches DISMISS_SETTLE_MS.
        let mut t = start;
        while t < start + Duration::from_millis(DIALOG_IDENTIFY_TIMEOUT_MS - 500) {
            t += Duration::from_millis(300);
            let redraw = "\x1b[2K\u{276f} No, exit\r\n  Yes, I trust this folder\r\n";
            seq.feed_at(redraw.as_bytes(), t);
            assert!(
                matches!(
                    seq.poll_timers_at(t + Duration::from_millis(50)),
                    StartupAction::None
                ),
                "keys must be held while the TUI keeps painting"
            );
        }
        // At the ceiling the session refuses loudly instead of guessing.
        match seq.poll_timers_at(start + Duration::from_millis(DIALOG_IDENTIFY_TIMEOUT_MS)) {
            StartupAction::Refuse(reason) => {
                assert!(
                    reason.contains("--pretrust-cwd"),
                    "refusal must be actionable: {reason}"
                )
            }
            other => panic!("expected Refuse at the identify ceiling, got {other:?}"),
        }
    }

    #[test]
    fn feed_no_dialog_output_returns_none() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let action = seq.feed(b"Starting Claude Code...\n");
        assert!(matches!(action, StartupAction::None));
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
    }

    #[test]
    fn feed_trust_dismissed_phase_ignored() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        seq.feed(DIALOG_2_1_263.as_bytes());
        seq.poll_timers_at(Instant::now() + Duration::from_millis(DISMISS_SETTLE_MS));
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
        // Additional output in TrustDismissed phase must be ignored.
        let action = seq.feed(b"trust Allow folder permission proceed\n");
        assert!(matches!(action, StartupAction::None));
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
    }

    #[test]
    fn feed_dialog_split_across_chunks_dismisses_once_options_arrive() {
        let mut seq = StartupSeq::new(b"hello".to_vec());
        let a1 =
            seq.feed(b"Quick safety check: Is this a project you created or one you trust?\r\n");
        assert!(matches!(a1, StartupAction::None));
        let a2 = seq.feed("\u{276f} No, exit\r\n".as_bytes());
        assert!(
            matches!(a2, StartupAction::None),
            "trusting entry not rendered yet"
        );
        let a3 = seq.feed(b"  Yes, I trust this folder\r\n");
        assert!(
            matches!(a3, StartupAction::None),
            "trusting entry identified but the TUI has not gone quiet yet, got {a3:?}"
        );
        match seq.poll_timers_at(Instant::now() + Duration::from_millis(DISMISS_SETTLE_MS)) {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => {
                panic!("expected dismissal keys once the quiet window passes, got {other:?}")
            }
        }
    }

    // ── refusal of an unidentifiable dialog ───────────────────────────────────

    /// A dialog that never identifies its trusting entry must NOT be confirmed
    /// by a bare CR: the fallback Enter would select the highlighted default,
    /// which is "No, exit" in claude 2.1.263.
    #[test]
    fn unresolved_dialog_refuses_instead_of_confirming_default() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;
        // Trust keywords arrive but no Yes/No options ever render.
        seq.feed_at(b"Do you trust and Allow this folder?\r\n", start);

        // Quiet but not yet settled → still waiting, never a bare CR.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(400)),
            StartupAction::None
        ));
        assert_eq!(*seq.phase(), StartupPhase::Waiting);

        // Settled → refuse loudly with a distinct diagnostic.
        match seq.poll_timers_at(start + Duration::from_millis(DIALOG_SETTLE_MS)) {
            StartupAction::Refuse(reason) => {
                assert!(
                    reason.contains("--pretrust-cwd"),
                    "refusal must point at the escape hatch: {reason}"
                );
                assert!(
                    reason.contains("Do you trust and Allow this folder?"),
                    "refusal must quote the rendered output: {reason}"
                );
            }
            other => panic!("expected Refuse for an unidentifiable dialog, got {other:?}"),
        }
    }

    /// New output keeps the dialog from being declared settled: only genuine
    /// silence (or the identify deadline) may refuse.
    #[test]
    fn unresolved_dialog_waits_while_tui_renders() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;
        seq.feed_at(b"Do you trust and Allow this folder?\r\n", start);

        for ms in [500u64, 1000, 1500, 1900] {
            let redraw = "\x1b[2Krendering…\r\n";
            seq.feed_at(redraw.as_bytes(), start + Duration::from_millis(ms));
            assert!(
                matches!(
                    seq.poll_timers_at(start + Duration::from_millis(ms + 100)),
                    StartupAction::None
                ),
                "active rendering must defer the refusal at {ms}ms"
            );
        }

        // The absolute identify deadline fires even with output still arriving.
        match seq.poll_timers_at(start + Duration::from_millis(DIALOG_IDENTIFY_TIMEOUT_MS)) {
            StartupAction::Refuse(_) => {}
            other => panic!("identify deadline must refuse, got {other:?}"),
        }
    }

    // ── no-dialog fallbacks (trusted cwd) ─────────────────────────────────────

    #[test]
    fn idle_fallback_fires_after_400ms_of_silence() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;

        assert!(matches!(
            seq.feed_at(&[b'x'; IDLE_THRESHOLD_BYTES], start),
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

        seq.feed_at(&[b'x'; IDLE_THRESHOLD_BYTES], start);
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

    /// A dialog on screen suppresses the no-dialog idle fallback: 400ms of
    /// quiet must not send a bare CR that would confirm "No, exit".
    #[test]
    fn idle_fallback_suppressed_while_dialog_unresolved() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;
        let dialog = concat!(
            "Quick safety check: Is this a project you created or one you trust?\r\n",
            "\u{276f} No, exit\r\n",
        );
        seq.feed_at(dialog.as_bytes(), start);

        assert!(
            matches!(
                seq.poll_timers_at(start + Duration::from_millis(IDLE_TIMEOUT_MS)),
                StartupAction::None
            ),
            "400ms of quiet must not bare-confirm an unresolved dialog"
        );
        assert_eq!(*seq.phase(), StartupPhase::Waiting);
    }

    // ── idle-gap timer tests ──────────────────────────────────────────────────

    #[test]
    fn default_post_dismiss_output_restarts_one_second_quiet_window() {
        let mut seq = StartupSeq::new(b"prompt".to_vec());
        let start = seq.phase_start;

        seq.feed_at(DIALOG_2_1_263.as_bytes(), start);
        // The dismissal is identified at once but held for the post-render
        // quiet window; it fires when the TUI has been quiet long enough.
        match seq.poll_timers_at(start + Duration::from_millis(DISMISS_SETTLE_MS)) {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);
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

        // Trigger trust dismiss.
        seq.feed(DIALOG_2_1_263.as_bytes());
        // The dismissal is identified but held for the post-render quiet
        // window; sleep past it and poll the keys out.
        std::thread::sleep(Duration::from_millis(DISMISS_SETTLE_MS + 20));
        match seq.poll_timers() {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
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
        seq.feed(DIALOG_2_1_263.as_bytes());
        // The dismissal is identified instantly but held while the TUI may
        // still be painting, so nothing fires before the quiet window does.
        let action = seq.poll_timers();
        assert!(
            matches!(action, StartupAction::None),
            "identified keys are held until the TUI has been quiet"
        );

        // Wait past the dismiss quiet window; the dismissal keys fire first.
        std::thread::sleep(Duration::from_millis(DISMISS_SETTLE_MS + 10));
        match seq.poll_timers() {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // Wait for the post-dismiss idle gap of silence.
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

        seq.feed(DIALOG_2_1_263.as_bytes());

        // First poll after the quiet window → dismissal keys.
        std::thread::sleep(Duration::from_millis(DISMISS_SETTLE_MS + 10));
        let a1 = seq.poll_timers();
        assert!(
            matches!(a1, StartupAction::Write(_)),
            "expected dismissal keys after the quiet window, got {a1:?}"
        );
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // Second poll past the idle gap → inject.
        std::thread::sleep(Duration::from_millis(gap_ms + 10));
        let a2 = seq.poll_timers();
        assert!(matches!(a2, StartupAction::Write(_)));

        // Subsequent polls must be None.
        let a3 = seq.poll_timers();
        assert!(matches!(a3, StartupAction::None));
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

        seq.feed(DIALOG_2_1_263.as_bytes());

        // The dismissal keys fire after the post-render quiet window.
        std::thread::sleep(Duration::from_millis(DISMISS_SETTLE_MS + 10));
        match seq.poll_timers() {
            StartupAction::Write(keys) => assert_eq!(keys, b"\x1b[B\r"),
            other => panic!("expected dismissal keys after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // Then the idle gap of silence injects the prompt.
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

    // ── prewarmed (ADR-005 pool path) ─────────────────────────────────────────
    //
    // The daemon hands over a worker whose REPL is already past trust-dismiss
    // and idle-settled. The sequencer must start at TrustDismissed: it owes
    // only the injection quiet window, and it must never re-enter the Waiting
    // decision paths (dialog scan, hard timeout, bare-CR idle fallback) — an
    // idle prewarmed REPL would trip the 45 s hard timeout and a bare CR would
    // submit an empty turn.

    #[test]
    fn prewarmed_starts_at_trust_dismissed() {
        let seq = StartupSeq::prewarmed(b"p".to_vec());
        assert_eq!(
            *seq.phase(),
            StartupPhase::TrustDismissed,
            "a prewarmed worker is already past trust-dismiss"
        );
    }

    #[test]
    fn prewarmed_ignores_dialog_looking_output() {
        // Post-handoff repaints (or anything dialog-shaped) must not re-enter
        // the Waiting-phase scanner: feed() provably scans nothing outside
        // Waiting, so this holds by construction — pin it anyway, since a
        // future refactor that lets prewarmed start in Waiting would silently
        // reintroduce both the bare-CR and hard-timeout failure modes.
        let gap_ms: u64 = 1_000;
        let mut seq = StartupSeq::prewarmed_with_idle_gap(b"p".to_vec(), gap_ms);
        let start = Instant::now();

        // A full trust dialog arrives after handoff. No dismissal keys may be
        // produced — the scanner does not run on this path.
        let action = seq.feed_at(DIALOG_2_1_263.as_bytes(), start);
        assert!(
            matches!(action, StartupAction::None),
            "no dismissal keys may be produced on the prewarmed path, got {action:?}"
        );
        assert_eq!(*seq.phase(), StartupPhase::TrustDismissed);

        // The output restarted the quiet window (feed updates last_output_at
        // before the phase check), so nothing fires inside it.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(gap_ms - 1)),
            StartupAction::None
        ));

        // Well past every Waiting-phase deadline (hard timeout 45 s, dialog
        // identify ceiling, idle fallback) AND past the quiet window: the ONLY
        // thing that may fire is the bracketed-paste prompt injection — never
        // dismissal keys, never a bare CR.
        match seq.poll_timers_at(start + Duration::from_secs(60)) {
            StartupAction::Write(payload) => {
                assert!(
                    payload.starts_with(b"\x1b[200~"),
                    "expected bracketed-paste injection, got {payload:?}"
                );
                assert_ne!(
                    payload, b"\r",
                    "the Waiting-phase idle fallback's bare CR must never fire here"
                );
            }
            other => panic!("expected prompt injection, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);
    }

    #[test]
    fn prewarmed_injects_after_quiet_window_without_bare_cr() {
        let gap_ms: u64 = 1_000;
        let mut seq = StartupSeq::prewarmed_with_idle_gap(b"hello pool".to_vec(), gap_ms);
        let start = Instant::now();

        // Before the quiet window elapses: nothing is written to the worker.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(gap_ms - 1)),
            StartupAction::None
        ));

        // At the quiet window: the bracketed-paste payload, never a bare CR.
        match seq.poll_timers_at(start + Duration::from_millis(gap_ms)) {
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
                    payload.windows(10).any(|w| w == b"hello pool"),
                    "prompt text not present in payload"
                );
                assert_ne!(
                    payload, b"\r",
                    "the Waiting-phase idle fallback's bare CR must never reach a prewarmed REPL"
                );
            }
            other => panic!("expected prompt injection after the quiet window, got {other:?}"),
        }
        assert_eq!(*seq.phase(), StartupPhase::PromptInjected);

        // And the sequencer is spent, like every injected state.
        assert!(matches!(seq.poll_timers(), StartupAction::None));
    }

    #[test]
    fn prewarmed_quiet_window_restarts_on_worker_output() {
        // Post-handoff repaint output restarts the injection quiet window,
        // exactly as it does on the stateless post-dismiss path: bytes are
        // never written into a TUI that is still painting.
        let gap_ms: u64 = 1_000;
        let mut seq = StartupSeq::prewarmed_with_idle_gap(b"p".to_vec(), gap_ms);
        let start = Instant::now();

        seq.feed_at(b"\x1b[2Krepaint\r\n", start + Duration::from_millis(500));

        // 600 ms of silence after the repaint: < gap_ms since last output.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(1_100)),
            StartupAction::None
        ));

        // A full gap after the repaint: inject.
        assert!(matches!(
            seq.poll_timers_at(start + Duration::from_millis(1_501)),
            StartupAction::Write(_)
        ));
    }
}
