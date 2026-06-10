use crate::error::{Error, Result};
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
    Text { text: String },
    ToolUse { name: String },
    Thinking { thinking: String },
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
    Assistant { message: AssistantMessage },
    User { message: serde_json::Value },
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

/// Read a transcript with retry loop and fallback.
///
/// Retries up to 40×50 ms when the file is missing or text is empty (Stop-before-JSONL race
/// window, PO-5). Falls back to `last_assistant_message` if retries are exhausted.
/// Returns an error if both are empty.
pub fn read_transcript(
    path: &Path,
    last_assistant_message: Option<&str>,
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
                return Ok(r);
            }
        }
    }

    if let Some(msg) = last_assistant_message.filter(|s| !s.is_empty()) {
        return Ok(TranscriptResult {
            text: msg.to_string(),
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
