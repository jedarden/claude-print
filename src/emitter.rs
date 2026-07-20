use crate::cli::OutputFormat;
use crate::error::ClaudePrintError;
use crate::transcript::{strip_ansi, TranscriptResult};
use std::borrow::Cow;
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
    // EC-9: defense-in-depth sanitizer for the `last_assistant_message` fallback
    // path. `read_transcript()` already strips ANSI from the fallback string at
    // its source, so for normal operation this is a no-op (the `Borrowed` arm —
    // zero allocation). It guarantees a `TranscriptResult` built with
    // `used_fallback=true` — which tests and any future code path can construct
    // directly, bypassing `read_transcript` — can never leak raw ANSI escapes to
    // the caller's stdout in `text` or `json`. Normal JSONL-sourced text
    // (`used_fallback=false`) is emitted verbatim, never routed through the
    // strip. Stripping is idempotent, so double-application with the source strip
    // is harmless.
    let text: Cow<'_, str> = if result.used_fallback {
        Cow::Owned(strip_ansi(&result.text))
    } else {
        Cow::Borrowed(&result.text)
    };

    match format {
        OutputFormat::Text => {
            writeln!(writer, "{}", text)?;
        }
        OutputFormat::Json => {
            // bf-416c: read is_error from the transcript rather than hardcoding
            // false. Session::run() converts is_error:true transcripts into an
            // Err before we ever get here, so this is normally false — but
            // surfacing the real flag is defense in depth against any future
            // path that reaches emit_success with an errored transcript.
            let obj = serde_json::json!({
                "type": "result",
                "subtype": "success",
                "is_error": result.is_error,
                "result": text.as_ref(),
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
///
/// The reader thread is spawned at `PROMPT_INJECTED` and tails the transcript
/// file to stdout. It MUST be joined before the process exits on *every* exit
/// path (plan invariant INV-8): normal Stop completion, watchdog timeout,
/// SIGINT/SIGTERM, child-exit-without-Stop, and any `?` early return (e.g. a
/// transcript parse error). `main()` always terminates via `process::exit()`,
/// so an unjoined reader would be killed mid-write, truncating its output.
///
/// This is enforced by `Drop`, which disconnects the drain channel and joins
/// the thread — so simply letting the handle go out of scope (including via
/// `?` propagation) is always safe and never orphans the reader.
///
/// Drain vs. exit-immediately is the caller's choice:
/// - **Normal Stop transition:** call [`StreamJsonHandle::signal_drain`] first
///   so the reader forwards its remaining transcript lines before exiting.
/// - **Every other path (timeout, interrupt, error):** drop the handle without
///   signaling. `Drop` disconnects the channel; the reader treats `Disconnected`
///   as "exit immediately" and the join returns promptly.
#[derive(Debug)]
pub struct StreamJsonHandle {
    /// `Some` while the sender is held; `take()`n by `Drop` so the channel is
    /// disconnected *before* the join. (A field cannot be moved out of `&mut
    /// self`, so we wrap it in `Option` to release it explicitly — without this
    /// the channel would stay connected, the reader would never exit, and
    /// `join()` would hang.)
    drain_tx: Option<mpsc::SyncSender<()>>,
    /// `Some` while the handle is held; `take()`n by `Drop` so the join can
    /// *consume* it. `JoinHandle::join` takes `self` by value, so it cannot be
    /// called on `&mut self.join_handle` directly — without this `Option` the
    /// `Drop` impl would not compile (E0507: cannot move out of a field behind
    /// a mutable reference).
    join_handle: Option<thread::JoinHandle<()>>,
}

impl StreamJsonHandle {
    /// Signal the reader to forward its remaining transcript lines, then exit.
    ///
    /// Call this on the normal Stop transition. The sender survives the send,
    /// so the subsequent `Drop` is what disconnects the channel after the reader
    /// has observed the drain value.
    pub fn signal_drain(&self) {
        if let Some(tx) = &self.drain_tx {
            // sync_channel(1): one buffered slot. Ignore a WouldBlock (already signaled).
            let _ = tx.send(());
        }
    }
}

impl Drop for StreamJsonHandle {
    fn drop(&mut self) {
        // 1. Disconnect the channel FIRST. The reader polls try_recv every ~5ms;
        //    on Disconnected it returns immediately (no drain). This MUST happen
        //    before the join — otherwise the reader never exits and join() hangs.
        //    On the Stop path, signal_drain() already delivered the drain value,
        //    so the reader observes Ok(()), drains remaining lines, and only then
        //    sees the disconnect.
        self.drain_tx.take();
        // 2. Join so the caller is guaranteed the thread has fully exited — and
        //    all buffered stdout writes are flushed — before control returns
        //    (INV-8). `take()` moves the handle out of `&mut self` so `join`
        //    (which consumes it) can run; join() is a no-op if the thread
        //    already exited.
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
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
        drain_tx: Some(drain_tx),
        join_handle: Some(join_handle),
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
    // Per plan: retry with 50ms sleeps for up to 5 seconds.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let file = loop {
        match File::open(&transcript_path) {
            Ok(f) => break f,
            Err(_) => {
                // Check for drain signal or timeout
                match drain_rx.try_recv() {
                    Ok(()) => return,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {
                        // Check 5-second timeout
                        if std::time::Instant::now() >= deadline {
                            // Timeout expired - file never appeared
                            return;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
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
