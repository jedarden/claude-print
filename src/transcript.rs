use crate::error::{Error, Result};
use crate::verbose::Tracer;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::thread;
use std::time::Duration;

type UsageKey = (Option<u64>, Option<u64>, Option<u64>, Option<u64>);

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

impl Usage {
    fn as_key(&self) -> UsageKey {
        (
            self.input_tokens,
            self.output_tokens,
            self.cache_creation_input_tokens,
            self.cache_read_input_tokens,
        )
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct AggregatedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl AggregatedUsage {
    fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens.unwrap_or(0);
        self.output_tokens += usage.output_tokens.unwrap_or(0);
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens.unwrap_or(0);
        self.cache_read_input_tokens += usage.cache_read_input_tokens.unwrap_or(0);
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
    },
    Thinking {
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct AssistantMessage {
    pub id: Option<String>,
    pub content: Vec<ContentBlock>,
    pub usage: Usage,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ResultEvent {
    pub is_error: Option<bool>,
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    Assistant {
        message: AssistantMessage,
    },
    User {
        message: serde_json::Value,
    },
    Result(ResultEvent),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default)]
pub struct TranscriptResult {
    pub text: String,
    pub num_turns: usize,
    pub usage: AggregatedUsage,
    pub session_id: Option<String>,
    pub is_error: bool,
    pub used_fallback: bool,
}

/// Parse a transcript JSONL file once (no retry).
///
/// Missing files return an empty result. Malformed lines are silently skipped.
pub fn parse_transcript(path: &Path) -> Result<TranscriptResult> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TranscriptResult::default());
        }
        Err(e) => return Err(e.into()),
    };

    let reader = BufReader::new(file);
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut prev_usage_key: Option<UsageKey> = None;
    let mut agg_usage = AggregatedUsage::default();
    let mut num_turns: usize = 0;
    let mut current_turn_text = String::new();
    let mut session_id: Option<String> = None;
    let mut is_error = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }

        let event: Event = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match event {
            Event::Assistant { message } => {
                let is_new_turn = if let Some(id) = &message.id {
                    seen_ids.insert(id.clone())
                } else {
                    let key = message.usage.as_key();
                    let new = Some(&key) != prev_usage_key.as_ref();
                    prev_usage_key = Some(key);
                    new
                };

                if is_new_turn {
                    current_turn_text.clear();
                    num_turns += 1;
                    agg_usage.add(&message.usage);
                }

                for block in &message.content {
                    if let ContentBlock::Text { text } = block {
                        current_turn_text.push_str(text);
                    }
                }
            }
            Event::Result(r) => {
                if r.session_id.is_some() {
                    session_id = r.session_id;
                }
                is_error = r.is_error.unwrap_or(false);
            }
            Event::User { .. } | Event::Unknown => {}
        }
    }

    Ok(TranscriptResult {
        text: current_turn_text,
        num_turns,
        usage: agg_usage,
        session_id,
        is_error,
        used_fallback: false,
    })
}

/// Read a transcript with retry loop and fallback (no `--verbose` tracing).
///
/// Thin wrapper over [`read_transcript_traced`] with a disabled tracer, for
/// entry points that are not part of a session run (unit tests, standalone
/// tools). `session.rs` uses the traced variant so `--verbose` can surface the
/// retry count.
pub fn read_transcript(
    path: &Path,
    last_assistant_message: Option<&str>,
) -> Result<TranscriptResult> {
    read_transcript_traced(path, last_assistant_message, &Tracer::disabled())
}

/// Read a transcript with retry loop and fallback, emitting `--verbose` traces
/// for the retry count (plan §"`--verbose` Trace Points`").
///
/// Retries up to 40×50 ms when the file is missing or text is empty
/// (Stop-before-JSONL race window, PO-5). Falls back to `last_assistant_message`
/// if retries are exhausted. Returns an error if both are empty.
///
/// `tracer` makes the `--verbose` traces best-effort: a disabled tracer (the
/// default) turns every `trace` call into a cheap no-op, so the hot path is
/// unchanged when `--verbose` is off.
pub fn read_transcript_traced(
    path: &Path,
    last_assistant_message: Option<&str>,
    tracer: &Tracer,
) -> Result<TranscriptResult> {
    const MAX_RETRIES: usize = 40;
    const RETRY_DELAY: Duration = Duration::from_millis(50);

    let mut last_session_id: Option<String> = None;
    let mut last_is_error = false;

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            thread::sleep(RETRY_DELAY);
        }
        if let Ok(r) = parse_transcript(path) {
            if r.session_id.is_some() {
                last_session_id = r.session_id.clone();
            }
            last_is_error = r.is_error;
            if !r.text.is_empty() {
                // --verbose retry-count trace: `attempt + 1` is the 1-based read
                // number — 1 means success on the first try (no retries needed).
                tracer.trace(format!("transcript read on attempt {}", attempt + 1));
                return Ok(r);
            }
        }
    }

    if let Some(msg) = last_assistant_message.filter(|s| !s.is_empty()) {
        tracer.trace(format!(
            "transcript retry exhausted after {} attempts; using last_assistant_message fallback",
            MAX_RETRIES + 1
        ));
        return Ok(TranscriptResult {
            // EC-9: the Stop payload's `last_assistant_message` originates from
            // Claude Code's TUI-facing internals and may carry raw ANSI escapes
            // (SGR color codes, OSC title strings, cursor moves). Sanitize it
            // HERE — the single point where fallback text enters the system —
            // so every downstream consumer receives clean text. This is required
            // for the `stream-json` synthesized error result path: that flows
            // through `Error::AssistantError(t.text)` → `emit_error`, whose
            // signature carries only the message string and has no `used_fallback`
            // context to gate a strip on, so the strip must happen upstream.
            // The text/json emitter path also gets an idempotent defense-in-depth
            // strip in `emit_success`.
            text: strip_ansi(msg),
            num_turns: 0,
            usage: AggregatedUsage::default(),
            session_id: last_session_id,
            is_error: last_is_error,
            used_fallback: true,
        });
    }

    Err(Error::Internal(anyhow::anyhow!(
        "no response text after 40 retries; no last_assistant_message fallback"
    )))
}

/// Strip ANSI escape sequences from a string (EC-9).
///
/// Claude Code's TUI-facing internals can plausibly embed raw ANSI escapes —
/// SGR color codes (`ESC[31m`…`ESC[0m`), cursor moves, OSC title strings — in
/// the Stop payload's `last_assistant_message`. When the transcript retry loop
/// exhausts its budget and falls back to that string ([`read_transcript`]),
/// those codes must not leak to the caller's stdout in `text`/`json` output or
/// the `stream-json` synthesized error result. EC-9 scopes this to the fallback
/// string ONLY; normal JSONL-sourced assistant text is never routed through
/// here (see [`parse_transcript`]), so legitimate output is preserved verbatim.
///
/// The scan is byte-oriented. Every ANSI introducer is ASCII (`ESC` `0x1B` plus
/// ASCII control bytes), so removing such runs can never split a UTF-8 multibyte
/// sequence — the output is always valid UTF-8. Recognized sequences:
///
/// - **CSI** (`ESC [`): parameters `0x30–0x3F`, intermediates `0x20–0x2F`,
///   terminated by a final byte `0x40–0x7E` (covers SGR, cursor, erase, private
///   `?`-prefixed modes such as `ESC[?25l`).
/// - **OSC** (`ESC ]`): runs until `BEL` (`0x07`) or the String Terminator
///   (`ESC \`), matching both legacy BEL-terminated and ST-terminated forms.
/// - **Other escapes**: two-byte `ESC <byte>` (e.g. `ESC \`, `ESC M`) and the
///   intermediate-byte form `ESC` + `0x20–0x2F` + final (charset designators
///   like `ESC ( B`).
pub fn strip_ansi(input: &str) -> String {
    const ESC: u8 = 0x1B;
    const BEL: u8 = 0x07;

    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != ESC {
            out.push(bytes[i]);
            i += 1;
            continue;
        }

        // ESC seen — classify the byte that follows.
        i += 1;
        if i >= bytes.len() {
            break; // lone trailing ESC — drop it
        }
        match bytes[i] {
            b'[' => {
                // CSI: skip '[', then parameters/intermediates up to a final byte.
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    if (0x40..=0x7E).contains(&b) {
                        i += 1; // final byte — consume and end sequence
                        break;
                    } else if (0x20..=0x3F).contains(&b) {
                        i += 1; // parameter (0x30-0x3F) or intermediate (0x20-0x2F) byte
                    } else {
                        break; // unexpected byte — leave it for normal copy
                    }
                }
            }
            b']' => {
                // OSC: skip ']', then consume until BEL or any ESC. The ST is
                // `ESC \`; a bare ESC also ends OSC content and is left for the
                // outer loop to reprocess as a fresh escape.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == BEL {
                        i += 1;
                        break;
                    }
                    if bytes[i] == ESC {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2; // consume String Terminator (ESC \)
                        }
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                // Other escape. If the next byte is an intermediate (0x20-0x2F,
                // e.g. `(`/`)` for charset designators), consume it plus any
                // further intermediates and a final byte. Otherwise treat this
                // as a two-byte escape (`ESC <byte>`, e.g. `ESC \` or `ESC M`).
                if (0x20..=0x2F).contains(&bytes[i]) {
                    i += 1;
                    while i < bytes.len() && (0x20..=0x2F).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() && (0x30..=0x7E).contains(&bytes[i]) {
                        i += 1; // final byte
                    }
                } else {
                    i += 1; // two-byte escape: consume the single following byte
                }
            }
        }
    }

    // Removing ASCII-only runs from valid UTF-8 always yields valid UTF-8.
    String::from_utf8(out).expect("stripping ASCII ANSI codes preserves UTF-8 validity")
}
