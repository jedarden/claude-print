use crate::cli::OutputFormat;
use crate::error::ClaudePrintError;
use crate::transcript::TranscriptResult;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Emit a successful response.
///
/// `text`: writes `{response_text}\n` to stdout.
/// `json`: writes a single-line JSON result object.
/// `stream-json`: no-op — the reader thread handles all output.
pub fn emit_success(
    writer: &mut dyn Write,
    result: &TranscriptResult,
    format: &OutputFormat,
    claude_version: &str,
    duration_ms: u64,
) -> std::io::Result<()> {
    match format {
        OutputFormat::Text => {
            writeln!(writer, "{}", result.text)?;
        }
        OutputFormat::Json => {
            let obj = serde_json::json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": result.text,
                "session_id": result.session_id,
                "num_turns": result.num_turns as u64,
                "duration_ms": duration_ms,
                "cost_usd": 0,
                "claude_version": claude_version,
                "usage": {
                    "input_tokens": result.usage.input_tokens,
                    "output_tokens": result.usage.output_tokens,
                    "cache_creation_input_tokens": result.usage.cache_creation_input_tokens,
                    "cache_read_input_tokens": result.usage.cache_read_input_tokens,
                }
            });
            writeln!(writer, "{}", serde_json::to_string(&obj).unwrap())?;
        }
        OutputFormat::StreamJson => {
            // Reader thread handles all output; nothing to emit here on success.
        }
    }
    Ok(())
}

/// Emit an error result.
///
/// `text`: message to stderr only.
/// `json`: JSON error object to stdout.
/// `stream-json` after inject: JSON error object to stdout.
/// `stream-json` before inject: message to stderr only (same as text).
pub fn emit_error(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    error: &ClaudePrintError,
    format: &OutputFormat,
    claude_version: &str,
    stream_json_after_inject: bool,
) -> std::io::Result<()> {
    let write_json = match format {
        OutputFormat::Json => true,
        OutputFormat::StreamJson => stream_json_after_inject,
        OutputFormat::Text => false,
    };

    if write_json {
        let obj = serde_json::json!({
            "type": "result",
            "subtype": error.subtype(),
            "is_error": true,
            "error_message": error.message(),
            "claude_version": claude_version,
        });
        writeln!(stdout, "{}", serde_json::to_string(&obj).unwrap())?;
    } else {
        writeln!(stderr, "error: {}", error.message())?;
    }
    Ok(())
}

/// Handle for the stream-json reader thread.
pub struct StreamJsonHandle {
    /// Send `()` to signal "drain remaining lines then exit".
    /// Drop without sending to signal "exit immediately".
    pub drain_tx: mpsc::SyncSender<()>,
    pub join_handle: thread::JoinHandle<()>,
}

/// Spawn a stream-json reader thread writing to stdout.
pub fn spawn_stream_json_reader(transcript_path: PathBuf, start_offset: u64) -> StreamJsonHandle {
    spawn_stream_json_reader_to(transcript_path, start_offset, Box::new(std::io::stdout()))
}

/// Spawn a stream-json reader thread writing to the given writer (testable).
pub fn spawn_stream_json_reader_to(
    transcript_path: PathBuf,
    start_offset: u64,
    writer: Box<dyn Write + Send + 'static>,
) -> StreamJsonHandle {
    let (drain_tx, drain_rx) = mpsc::sync_channel(1);
    let join_handle = thread::spawn(move || {
        stream_json_reader_loop(transcript_path, start_offset, writer, drain_rx);
    });
    StreamJsonHandle {
        drain_tx,
        join_handle,
    }
}

fn stream_json_reader_loop(
    transcript_path: PathBuf,
    start_offset: u64,
    mut writer: Box<dyn Write + Send + 'static>,
    drain_rx: mpsc::Receiver<()>,
) {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    // Open the file, waiting if it doesn't exist yet.
    let file = loop {
        match File::open(&transcript_path) {
            Ok(f) => break f,
            Err(_) => match drain_rx.try_recv() {
                Ok(()) => return,
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => {
                    thread::sleep(Duration::from_millis(5));
                }
            },
        }
    };

    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(start_offset)).is_err() {
        let _ = drain_rx.recv();
        return;
    }

    let mut draining = false;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                if draining {
                    break;
                }
                match drain_rx.try_recv() {
                    Ok(()) => {
                        draining = true;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
                if !trimmed.is_empty() {
                    let _ = writeln!(writer, "{}", trimmed);
                }
            }
            Err(_) => {
                if draining {
                    break;
                }
                match drain_rx.try_recv() {
                    Ok(()) => draining = true,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
        }
    }
}
