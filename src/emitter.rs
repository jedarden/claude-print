use crate::cli::OutputFormat;
use crate::error::ClaudePrintError;
use crate::transcript::{strip_ansi, TranscriptResult};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

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
            // SAFETY: serde_json::to_string can only fail on very large data structures
            // or circular references. Our JSON objects are small, plain data structures,
            // so serialization cannot fail. We map any theoretical error to an IO error.
            writeln!(
                writer,
                "{}",
                serde_json::to_string(&obj).map_err(|e| {
                    std::io::Error::other(format!("JSON serialization failed: {}", e))
                })?
            )?;
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
/// `json`: JSON error object to stdout, except config errors go to stderr.
/// `stream-json` after inject: JSON error object to stdout, except config errors
/// go to stderr.
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
        if matches!(error, ClaudePrintError::Config(_)) {
            write_error_json(stderr, error, claude_version)?;
        } else {
            write_error_json(stdout, error, claude_version)?;
        }
    } else {
        writeln!(stderr, "error: {}", error.message())?;
    }
    Ok(())
}

fn write_error_json(
    writer: &mut dyn Write,
    error: &ClaudePrintError,
    claude_version: &str,
) -> std::io::Result<()> {
    let obj = serde_json::json!({
        "type": "result",
        "subtype": error.subtype(),
        "is_error": true,
        "error_message": error.message(),
        "claude_version": claude_version,
    });
    // SAFETY: serde_json::to_string can only fail on very large data structures
    // or circular references. Our JSON objects are small, plain data structures,
    // so serialization cannot fail. We map any theoretical error to an IO error.
    writeln!(
        writer,
        "{}",
        serde_json::to_string(&obj)
            .map_err(|e| { std::io::Error::other(format!("JSON serialization failed: {}", e)) })?
    )
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
/// This is enforced by `Drop`, which disconnects the channels and joins
/// the thread — so simply letting the handle go out of scope (including via
/// `?` propagation) is always safe and never orphans the reader.
///
/// Drain vs. exit-immediately is the caller's choice:
/// - **Normal Stop transition:** call [`StreamJsonHandle::retarget`] (binding
///   the reader to the transcript the Stop payload names) and then
///   [`StreamJsonHandle::signal_drain`] so the reader forwards the remainder
///   of the CORRECT transcript before exiting.
/// - **Every other path (timeout, interrupt, error):** drop the handle without
///   signaling. `Drop` disconnects the channels; the reader treats
///   `Disconnected` as "exit immediately" and the join returns promptly.
#[derive(Debug)]
pub struct StreamJsonHandle {
    /// `Some` while the sender is held; `take()`n by `Drop` so the channel is
    /// disconnected *before* the join. (A field cannot be moved out of `&mut
    /// self`, so we wrap it in `Option` to release it explicitly — without this
    /// the channel would stay connected, the reader would never exit, and
    /// `join()` would hang.)
    drain_tx: Option<mpsc::SyncSender<()>>,
    /// Bind/retarget channel: carries the transcript path the session wants
    /// the reader bound to (claudepr-a927ec0c). `take()`n by `Drop` together
    /// with `drain_tx`.
    retarget_tx: Option<mpsc::SyncSender<PathBuf>>,
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

    /// Bind (or re-bind) the reader to an exact transcript path.
    ///
    /// The Stop-payload backstop of the claudepr-a927ec0c design: the reader
    /// spawned at `PROMPT_INJECTED` binds from the per-drive identity file and
    /// only falls back to discovery when that is absent, but the Stop payload
    /// is the first AUTHORITATIVE statement of which transcript is ours. The
    /// session calls this right before [`Self::signal_drain`] with the
    /// resolved `transcript_path`:
    ///
    /// - reader already tailing this exact path (the normal identity-bound
    ///   run) → no-op: no offset reset, no duplicate forwarding;
    /// - reader bound to a DIFFERENT file (a legacy identity-less run that
    ///   mis-discovered under concurrency) or still unbound (ambiguity
    ///   refusal) → swap to this path at the injection-snapshot offset, so the
    ///   drained tail carries THIS session's final events, result event
    ///   included. Lines already forwarded from a wrong file cannot be
    ///   retracted — that residue is the documented bound of the fallback.
    ///
    /// Ordering with `signal_drain` matters: the reader checks a pending
    /// retarget BEFORE a pending drain at every idle tick, so a retarget sent
    /// first is always honored before the drain lets it exit.
    pub fn retarget(&self, transcript_path: PathBuf) {
        if let Some(tx) = &self.retarget_tx {
            // try_send on the capacity-1 channel: a retarget already pending in
            // the buffer wins (the reader consumes it at the next idle tick),
            // and this call must never block the session thread.
            let _ = tx.try_send(transcript_path);
        }
    }
}

impl Drop for StreamJsonHandle {
    fn drop(&mut self) {
        // 1. Disconnect the channels FIRST. The reader polls try_recv every
        //    ~5ms; on Disconnected it returns immediately (no drain). This MUST
        //    happen before the join — otherwise the reader never exits and
        //    join() hangs. On the Stop path, signal_drain() already delivered
        //    the drain value, so the reader observes Ok(()), drains remaining
        //    lines, and only then sees the disconnect.
        self.drain_tx.take();
        self.retarget_tx.take();
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

/// Snapshot every `.jsonl` in `dir` to its current byte size.
///
/// Captured at the `PROMPT_INJECTED` transition (before the reader is spawned)
/// and handed to the discovery reader as `pre_existing`. A file present in this
/// snapshot is an ONGOING session's transcript whose pre-injection bytes
/// (SessionStart, system messages) the reader must seek past; a file ABSENT from
/// it is one created after injection and tailed from offset 0. A missing `dir`
/// (claude has not yet created the projects directory) yields an empty map —
/// discovery then waits for both the directory and the file to appear.
pub fn snapshot_jsonl_sizes(dir: &Path) -> HashMap<PathBuf, u64> {
    let mut sizes = HashMap::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Ok(meta) = entry.metadata() {
                    sizes.insert(path, meta.len());
                }
            }
        }
    }
    sizes
}

/// Spawn a stream-json reader thread writing to stdout, tailing a known path.
pub fn spawn_stream_json_reader(transcript_path: PathBuf, start_offset: u64) -> StreamJsonHandle {
    spawn_stream_json_reader_to(transcript_path, start_offset, Box::new(std::io::stdout()))
}

/// Spawn a stream-json reader thread writing to the given writer (testable),
/// tailing a known path.
pub fn spawn_stream_json_reader_to(
    transcript_path: PathBuf,
    start_offset: u64,
    writer: Box<dyn Write + Send + 'static>,
) -> StreamJsonHandle {
    spawn_reader(
        TranscriptSource::Exact {
            path: transcript_path,
            start_offset,
        },
        writer,
    )
}

/// Spawn a stream-json reader that BINDS to this session's transcript via the
/// per-drive identity file, then tails it to stdout.
///
/// Spawned at `PROMPT_INJECTED`, where the `session_id` — and thus the exact
/// transcript filename `<session_id>.jsonl` — is still unknown (it arrives only
/// in the Stop payload, after injection). The reader therefore cannot be handed
/// a final path; instead it polls `identity_path` (claudepr-a927ec0c): the
/// UserPromptSubmit relay hook writes this session's `session_id` /
/// `transcript_path` there the instant the prompt is submitted, BEFORE any
/// assistant event exists, and the reader tails exactly that file.
///
/// Until a binding exists the reader forwards NOTHING — under same-cwd
/// concurrency the newest-growing `.jsonl` can be a SIBLING session's, and the
/// old newest-mtime guess forwarded that sibling wholesale. See
/// [`spawn_stream_json_reader_bound_to`] for the full binding ladder; the only
/// guess it still makes is a single transcript created after injection, which
/// no sibling started before us can produce.
///
/// `first_output` is the watchdog's Phase-2 credit flag
/// ([`crate::watchdog::WatchdogState::stream_json_output_flag`]): the reader
/// stores it the moment it forwards its first transcript line, making the
/// stream-json first-output deadline a true FIRST-OUTPUT deadline
/// (claudepr-33fdf4ed) instead of the unconditional session cap it was when
/// nothing could credit it.
pub fn spawn_stream_json_reader_bound(
    identity_path: PathBuf,
    projects_dir: PathBuf,
    pre_existing: HashMap<PathBuf, u64>,
    first_output: Option<Arc<AtomicBool>>,
) -> StreamJsonHandle {
    spawn_stream_json_reader_bound_to(
        identity_path,
        projects_dir,
        pre_existing,
        Box::new(std::io::stdout()),
        first_output,
    )
}

/// Testable variant of [`spawn_stream_json_reader_bound`] writing to `writer`.
///
/// Binding ladder, polled every 50ms until one resolves (or the reader is
/// told to exit):
///
/// 1. **Identity** — `identity_path` parses to a usable payload; the reader
///    binds to its `transcript_path` (preferred) or
///    `<projects_dir>/<session_id>.jsonl` (fallback), at the injection-time
///    size for a file present in `pre_existing` (skip pre-injection bytes of
///    an ongoing session's file) and 0 otherwise. The UserPromptSubmit hook
///    fires before the first assistant event exists, so this is the normal
///    path and live forwarding is unaffected.
/// 2. **Unambiguous new candidate** — the identity-less fallback (a claude
///    without UserPromptSubmit hook support): exactly one `.jsonl` created
///    after the injection snapshot is bound, once it has stayed sole for
///    [`IDENTITY_GRACE`] — identity keeps outranking it at every tick, so a
///    payload landing mid-grace wins. See
///    [`resolve_unambiguous_new_binding`] for why "created after injection"
///    (not the old new-or-grown rule) is what makes a single candidate
///    unambiguous under same-cwd concurrency.
/// 3. **Stop-payload retarget** — the authoritative backstop when neither
///    rung above resolves (no identity, and zero or several new candidates):
///    [`StreamJsonHandle::retarget`] delivers the resolved `StopInfo` path,
///    binding from the snapshot offset. An identity-less ambiguous run
///    therefore forwards NOTHING live and its output arrives whole — correct
///    and uncontaminated — at the drain; lines already forwarded from a
///    wrongly-fallback-bound file cannot be retracted, the documented bound
///    of the fallback.
pub fn spawn_stream_json_reader_bound_to(
    identity_path: PathBuf,
    projects_dir: PathBuf,
    pre_existing: HashMap<PathBuf, u64>,
    writer: Box<dyn Write + Send + 'static>,
    first_output: Option<Arc<AtomicBool>>,
) -> StreamJsonHandle {
    spawn_reader(
        TranscriptSource::Bind {
            identity_path,
            projects_dir,
            pre_existing,
            first_output,
        },
        writer,
    )
}

/// How the reader locates the transcript file to tail.
#[derive(Debug)]
enum TranscriptSource {
    /// A concrete, already-known path. The reader opens it directly, retrying for
    /// up to 5s if it does not exist yet, then seeks to `start_offset`.
    Exact { path: PathBuf, start_offset: u64 },
    /// Bind to this session's transcript at runtime (see
    /// [`spawn_stream_json_reader_bound_to`]). Used at `PROMPT_INJECTED`, where
    /// the exact `<session_id>.jsonl` filename is unknown. `first_output` is
    /// the watchdog's Phase-2 credit flag, stored on the first forwarded line.
    Bind {
        identity_path: PathBuf,
        projects_dir: PathBuf,
        pre_existing: HashMap<PathBuf, u64>,
        first_output: Option<Arc<AtomicBool>>,
    },
}

fn spawn_reader(
    source: TranscriptSource,
    writer: Box<dyn Write + Send + 'static>,
) -> StreamJsonHandle {
    let (drain_tx, drain_rx) = mpsc::sync_channel(1);
    let (retarget_tx, retarget_rx) = mpsc::sync_channel(1);
    let join_handle = thread::spawn(move || {
        stream_json_reader_loop(source, writer, drain_rx, retarget_rx);
    });
    StreamJsonHandle {
        drain_tx: Some(drain_tx),
        retarget_tx: Some(retarget_tx),
        join_handle: Some(join_handle),
    }
}

fn stream_json_reader_loop(
    source: TranscriptSource,
    writer: Box<dyn Write + Send + 'static>,
    drain_rx: mpsc::Receiver<()>,
    retarget_rx: mpsc::Receiver<PathBuf>,
) {
    let (initial_path, initial_offset, pre_existing, identity, first_output) = match source {
        TranscriptSource::Exact { path, start_offset } => {
            (path, start_offset, HashMap::new(), None, None)
        }
        TranscriptSource::Bind {
            identity_path,
            projects_dir,
            pre_existing,
            first_output,
        } => {
            // Resolve the transcript file and the byte offset to seek to.
            // Polls until a binding resolves; `None` → the reader was told to
            // drain/exit before anything bound: nothing to forward, return.
            // No timeout: the session lifetime bounds the wait, and the
            // Stop-payload retarget resolves even a never-identifying run.
            match bind_with_retry(
                &identity_path,
                &projects_dir,
                &pre_existing,
                &drain_rx,
                &retarget_rx,
            ) {
                Some((path, offset)) => (
                    path,
                    offset,
                    pre_existing,
                    Some((identity_path, projects_dir)),
                    first_output,
                ),
                None => return,
            }
        }
    };

    tail_loop(
        initial_path,
        initial_offset,
        &pre_existing,
        identity,
        writer,
        &drain_rx,
        &retarget_rx,
        first_output,
    );
}

/// Poll for the first binding (identity, then the identity-less unambiguous
/// fallback, then the Stop-payload retarget), 50ms ticks, until one resolves
/// or the reader is told to exit.
fn bind_with_retry(
    identity_path: &Path,
    projects_dir: &Path,
    pre_existing: &HashMap<PathBuf, u64>,
    drain_rx: &mpsc::Receiver<()>,
    retarget_rx: &mpsc::Receiver<PathBuf>,
) -> Option<(PathBuf, u64)> {
    // First sighting of the current lone new candidate (run 2's grace timer,
    // see [`IDENTITY_GRACE`]); reset whenever the scan stops agreeing.
    let mut sole_since: Option<(PathBuf, Instant)> = None;
    loop {
        // A pending Stop-payload retarget outranks everything: the session
        // reached Stop, and the payload is authoritative. Offset per the
        // snapshot rules — for a file that existed at injection the
        // pre-injection bytes are skipped, otherwise the file is tailed whole.
        if let Some(path) = take_retarget(retarget_rx, None) {
            let offset = snapshot_offset(&path, pre_existing);
            return Some((path, offset));
        }
        // Identity outranks the fallback, so a payload landing mid-grace binds
        // immediately instead of leaving the fallback to mature onto a file
        // identity is about to contradict.
        if let Some(path) = resolve_identity_binding(identity_path, projects_dir) {
            let offset = snapshot_offset(&path, pre_existing);
            return Some((path, offset));
        }
        // Identity-less fallback: bind only when exactly ONE transcript was
        // created after the injection snapshot (claudepr-a927ec0c rung 2),
        // and only once it has stayed sole for the grace window.
        if let Some(path) =
            resolve_unambiguous_new_binding(projects_dir, pre_existing, &mut sole_since)
        {
            let offset = snapshot_offset(&path, pre_existing);
            return Some((path, offset));
        }
        match drain_rx.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The byte offset a newly-bound transcript starts at: its injection-time size
/// when it was already present (skip pre-injection bytes of an ongoing
/// session's file), 0 for a file created after injection.
fn snapshot_offset(path: &Path, pre_existing: &HashMap<PathBuf, u64>) -> u64 {
    pre_existing.get(path).copied().unwrap_or(0)
}

/// Read the per-drive identity file and resolve the transcript path it names.
///
/// `None` while the file is absent, mid-write (empty or unparseable — the hook
/// may still be writing), or unusable (sparse payload without either field);
/// the caller simply polls again. `transcript_path` is preferred — the exact
/// path claude itself reports; `session_id` joins the same projects dir the
/// reader was handed.
fn resolve_identity_binding(identity_path: &Path, projects_dir: &Path) -> Option<PathBuf> {
    let bytes = fs::read(identity_path).ok()?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let payload = crate::poller::parse_stop_payload(&bytes).ok()?;
    let explicit = payload
        .transcript_path
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    explicit.or_else(|| {
        payload
            .session_id
            .filter(|s| !s.is_empty())
            .map(|sid| projects_dir.join(format!("{sid}.jsonl")))
    })
}

/// How long a lone new transcript must remain the ONLY new candidate before
/// the identity-less fallback binds it (claudepr-a927ec0c rung 2).
///
/// The grace closes the misbind race a raw scan has: a sibling that started
/// AFTER us also produces a new file, and in the window before our own
/// transcript exists a bare scan would see exactly one new candidate — the
/// sibling's — and bind it. Holding the bind until the sole candidate has
/// stayed sole for this long gives a landing identity the first chance at
/// every tick in between (identity is polled BEFORE the fallback), and a
/// sibling's second file turns the scan ambiguous and resets the wait. With
/// working hooks identity lands within milliseconds of prompt submission, so
/// the normal run never pays the grace; only a legacy identity-less claude
/// waits it out before live forwarding starts.
const IDENTITY_GRACE: Duration = Duration::from_millis(250);

/// The identity-less fallback binding (claudepr-a927ec0c rung 2): scan
/// `projects_dir` and return the single `.jsonl` created after the injection
/// snapshot — but only a candidate that has now been scanned as the SOLE new
/// candidate for at least [`IDENTITY_GRACE`] (`sole_since` tracks the
/// first sighting; the caller resets it whenever the scan result changes).
///
/// Candidates are files ABSENT from `pre_existing` — claude-print drives fresh
/// sessions, so this session's transcript is always a new file; a file present
/// in the snapshot belongs to a session OLDER than ours (its post-injection
/// growth is another session's live turn) and is never a candidate. A sibling
/// started after us contributes a SECOND new file, which returns `None` (the
/// ambiguity the old newest-mtime discovery guessed wrong); the grace window
/// additionally covers the sub-tick window where the sibling's file exists and
/// ours does not yet. `None` → not yet bound: the caller keeps polling, and
/// the Stop-payload retarget resolves even a never-identifying run at drain.
fn resolve_unambiguous_new_binding(
    projects_dir: &Path,
    pre_existing: &HashMap<PathBuf, u64>,
    sole_since: &mut Option<(PathBuf, Instant)>,
) -> Option<PathBuf> {
    let entries = fs::read_dir(projects_dir).ok()?;
    let mut candidate: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if pre_existing.contains_key(&path) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if candidate.is_some() {
            // Two or more new transcripts: attribution is impossible — the
            // refusal IS the fix (the old rule picked the newest and got the
            // sibling). The Stop retarget resolves this run at drain.
            *sole_since = None;
            return None;
        }
        candidate = Some(path);
    }
    match candidate {
        Some(path) => {
            let now = Instant::now();
            let matured = matches!(sole_since.as_ref(), Some((p, since)) if *p == path && now.duration_since(*since) >= IDENTITY_GRACE);
            if matured {
                *sole_since = None;
                return Some(path);
            }
            sole_since.get_or_insert((path, now));
            None
        }
        None => {
            *sole_since = None;
            None
        }
    }
}

/// Consume a pending retarget request, if any.
///
/// A request naming the path the reader is already bound to is consumed and
/// dropped (already correct — re-binding to the same path at its snapshot
/// offset would duplicate already-forwarded lines); `current` is that bound
/// path, `None` while unbound.
fn take_retarget(retarget_rx: &mpsc::Receiver<PathBuf>, current: Option<&Path>) -> Option<PathBuf> {
    match retarget_rx.try_recv() {
        Ok(path) if current == Some(path.as_path()) => None,
        Ok(path) => Some(path),
        Err(_) => None,
    }
}

/// Outcome of pointing the tail at a newly-bound path.
enum Rebind {
    /// `reader`/`path` now name the new file, positioned at its snapshot
    /// offset; the caller continues tailing.
    Swapped,
    /// The new file never opened within the retry budget — fall through to
    /// the normal drain/shutdown checks on the OLD reader.
    OpenFailed,
    /// Seek into the new file failed; the tail is over — block on the drain
    /// and exit, like every other fatal tail error.
    Fatal,
}

/// Bind the tail to `new_path`: open it, seek to its snapshot offset, and
/// point `reader`/`path` at it. Shared by the Stop-payload retarget and the
/// post-bind identity re-check (claudepr-a927ec0c).
fn rebind(
    reader: &mut std::io::BufReader<std::fs::File>,
    path: &mut PathBuf,
    new_path: PathBuf,
    pre_existing: &HashMap<PathBuf, u64>,
    drain_rx: &mpsc::Receiver<()>,
) -> Rebind {
    use std::io::{BufReader, Seek, SeekFrom};
    let Some(file) = open_with_retry(|| std::fs::File::open(&new_path).ok(), drain_rx) else {
        return Rebind::OpenFailed;
    };
    let mut new_reader = BufReader::new(file);
    let offset = snapshot_offset(&new_path, pre_existing);
    if new_reader.seek(SeekFrom::Start(offset)).is_err() {
        return Rebind::Fatal;
    }
    *reader = new_reader;
    *path = new_path;
    Rebind::Swapped
}

/// Open, seek, and forward transcript lines until drained — retargeting to a
/// newly-bound path whenever the session asks.
///
/// `first_output` is the watchdog's Phase-2 credit flag; it is consumed (set
/// once, then dropped) the moment the FIRST transcript line is forwarded, so
/// the stream-json first-output deadline measures real forwarded output
/// (claudepr-33fdf4ed).
#[allow(clippy::too_many_arguments)] // tail state; grouping would obscure the loop's inputs
fn tail_loop(
    initial_path: PathBuf,
    initial_offset: u64,
    pre_existing: &HashMap<PathBuf, u64>,
    identity: Option<(PathBuf, PathBuf)>,
    mut writer: Box<dyn Write + Send + 'static>,
    drain_rx: &mpsc::Receiver<()>,
    retarget_rx: &mpsc::Receiver<PathBuf>,
    mut first_output: Option<Arc<AtomicBool>>,
) {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let mut path = initial_path;
    let mut reader = match open_with_retry(|| File::open(&path).ok(), drain_rx) {
        Some(file) => BufReader::new(file),
        None => return,
    };
    if reader.seek(SeekFrom::Start(initial_offset)).is_err() {
        let _ = drain_rx.recv();
        return;
    }

    // Post-bind identity re-check (claudepr-a927ec0c): a Bind-source tail keeps
    // polling the identity file at the bind loop's 50ms cadence, so an identity
    // landing AFTER a fallback bind (a hook slower than [`IDENTITY_GRACE`])
    // still rebinds the tail onto the right transcript. Throttled — idle ticks
    // are 5ms and the check is a file read + JSON parse.
    let mut last_identity_poll = Instant::now();

    let mut draining = false;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // Idle tick. A pending retarget is honored BEFORE a pending
                // drain: the session sends retarget-then-drain, and honoring
                // them in that order is what makes the drained tail come from
                // the CORRECT transcript.
                if let Some(new_path) = take_retarget(retarget_rx, Some(&path)) {
                    match rebind(&mut reader, &mut path, new_path, pre_existing, drain_rx) {
                        Rebind::Swapped => continue,
                        // The retargeted file never opened or seek failed —
                        // nothing more to forward; fall through to the drain
                        // checks so the normal shutdown still applies.
                        Rebind::OpenFailed => {}
                        Rebind::Fatal => {
                            let _ = drain_rx.recv();
                            return;
                        }
                    }
                }
                // A late identity overrides a fallback binding (never the
                // Stop payload's: that arrives with `retarget`, above, and
                // the session stops polling identity once Stop resolves the
                // transcript itself).
                if let Some((identity_path, projects_dir)) = &identity {
                    if Instant::now().duration_since(last_identity_poll)
                        >= Duration::from_millis(50)
                    {
                        last_identity_poll = Instant::now();
                        if let Some(new_path) =
                            resolve_identity_binding(identity_path, projects_dir)
                        {
                            if new_path != path {
                                match rebind(
                                    &mut reader,
                                    &mut path,
                                    new_path,
                                    pre_existing,
                                    drain_rx,
                                ) {
                                    Rebind::Swapped => continue,
                                    Rebind::OpenFailed => {}
                                    Rebind::Fatal => {
                                        let _ = drain_rx.recv();
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
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
                    // Credit the watchdog's first-output deadline before the
                    // write: a forwarded line proves the child is producing
                    // stream-json events, which is exactly what Phase 2
                    // measures (claudepr-33fdf4ed). Consumed after the first
                    // line so the hot path pays nothing afterwards.
                    if let Some(flag) = first_output.take() {
                        flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    let _ = writeln!(writer, "{}", trimmed);
                }
            }
            Err(_) => {
                if let Some(new_path) = take_retarget(retarget_rx, Some(&path)) {
                    match rebind(&mut reader, &mut path, new_path, pre_existing, drain_rx) {
                        Rebind::Swapped => continue,
                        Rebind::OpenFailed => {}
                        Rebind::Fatal => {
                            let _ = drain_rx.recv();
                            return;
                        }
                    }
                }
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

/// Retry `attempt` every 50ms for up to 5s, honoring drain/shutdown.
///
/// `Ok(())` (drain) and `Disconnected` both bail: the file was never opened, so
/// there is nothing to drain. Returns `None` on timeout or shutdown.
fn open_with_retry<F>(mut attempt: F, drain_rx: &mpsc::Receiver<()>) -> Option<std::fs::File>
where
    F: FnMut() -> Option<std::fs::File>,
{
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(file) = attempt() {
            return Some(file);
        }
        match drain_rx.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    /// Collects everything the reader forwards so assertions can inspect it.
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture() -> (Arc<Mutex<Vec<u8>>>, Box<CaptureWriter>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = Box::new(CaptureWriter(Arc::clone(&buf)));
        (buf, writer)
    }

    fn forwarded(buf: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buf.lock().unwrap().clone()).unwrap()
    }

    /// Append verbatim marker lines (the reader forwards any non-empty line).
    fn append_markers(path: &Path, markers: &[&str]) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for m in markers {
            writeln!(f, r#"{{"marker":"{m}"}}"#).unwrap();
        }
    }

    fn write_identity(identity_path: &Path, payload: &str) {
        std::fs::write(identity_path, payload).unwrap();
    }

    fn spawn_bound(dir: &TempDir) -> (PathBuf, PathBuf, Arc<Mutex<Vec<u8>>>, StreamJsonHandle) {
        spawn_bound_with_flag(dir, None)
    }

    /// `spawn_bound` with a watchdog Phase-2 credit flag — for the
    /// claudepr-33fdf4ed first-output tests.
    fn spawn_bound_with_flag(
        dir: &TempDir,
        first_output: Option<Arc<AtomicBool>>,
    ) -> (PathBuf, PathBuf, Arc<Mutex<Vec<u8>>>, StreamJsonHandle) {
        let projects_dir = dir.path().join("projects").join("shared-cwd");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let identity_path = dir.path().join("session-identity.json");
        let pre_existing = snapshot_jsonl_sizes(&projects_dir);
        let (buf, writer) = capture();
        let handle = spawn_stream_json_reader_bound_to(
            identity_path.clone(),
            projects_dir.clone(),
            pre_existing,
            writer,
            first_output,
        );
        (identity_path, projects_dir, buf, handle)
    }

    /// The core contract of claudepr-a927ec0c: ZERO bytes are forwarded while
    /// the binding is unresolved. The wrong-file prefix cannot be retracted, so
    /// nothing may be emitted on a guess — not with two new candidates under
    /// same-cwd concurrency, not with an identity file that is empty (mid-write)
    /// or unparseable. Silence persists past [`IDENTITY_GRACE`], where the old
    /// newest-mtime discovery had already forwarded a sibling.
    #[test]
    fn bound_reader_forwards_nothing_while_identity_unresolved() {
        let dir = TempDir::new().unwrap();
        let (identity_path, projects_dir, buf, handle) = spawn_bound(&dir);

        // Two simultaneous new transcripts: attribution is impossible. The
        // ambiguity refusal IS the fix.
        append_markers(
            &projects_dir.join("sibling-a.jsonl"),
            &["SIBLING-A MUST NOT FORWARD"],
        );
        append_markers(
            &projects_dir.join("sibling-b.jsonl"),
            &["SIBLING-B MUST NOT FORWARD"],
        );

        // Past the identity-less grace window: still nothing, and none of the
        // unresolved identity shapes below may flip it to a guess.
        thread::sleep(IDENTITY_GRACE + Duration::from_millis(150));
        let text = forwarded(&buf);
        assert!(
            text.trim().is_empty(),
            "reader forwarded bytes with no positive bind (identity absent, \
             candidates ambiguous); got:\n{text}"
        );

        // Identity file present but mid-write (empty / whitespace): unresolved.
        write_identity(&identity_path, "  \n");
        thread::sleep(Duration::from_millis(200));
        assert!(
            forwarded(&buf).trim().is_empty(),
            "reader forwarded bytes while the identity file was mid-write"
        );

        // Identity file present but unparseable: unresolved.
        write_identity(&identity_path, "not json at all\n");
        thread::sleep(Duration::from_millis(200));
        assert!(
            forwarded(&buf).trim().is_empty(),
            "reader forwarded bytes while the identity file was unparseable"
        );

        // Dropping an unbound handle must terminate the bind poll (join inside
        // Drop; would hang the test otherwise).
        drop(handle);
        assert!(
            forwarded(&buf).trim().is_empty(),
            "bytes appeared without any positive bind"
        );
    }

    /// The binding must follow the identity payload's EXACT transcript_path —
    /// not the newest-mtime candidate. Two candidates grow while identity is
    /// unresolved (silence pinned mid-test); the payload then names the OLDER
    /// file, and only that file is forwarded, from its first byte.
    #[test]
    fn bound_reader_binds_identity_exact_path_not_newest_mtime() {
        let dir = TempDir::new().unwrap();
        let (identity_path, projects_dir, buf, handle) = spawn_bound(&dir);

        let ours_path = projects_dir.join("ours-session.jsonl");
        append_markers(&ours_path, &["OURS-FIRST-LINE"]);
        // Strictly newer mtime for the sibling: under the old newest-mtime rule
        // THIS is the file discovery would have forwarded.
        thread::sleep(Duration::from_millis(20));
        let sibling_path = projects_dir.join("sibling-newest.jsonl");
        append_markers(&sibling_path, &["SIBLING-NEWEST MUST NOT FORWARD"]);

        // Ambiguous + unresolved: silence.
        thread::sleep(Duration::from_millis(150));
        assert!(
            forwarded(&buf).trim().is_empty(),
            "reader guessed while identity was unresolved and candidates were \
             ambiguous; got:\n{}",
            forwarded(&buf)
        );

        // Identity resolves: it names OURS, the OLDER of the two candidates.
        write_identity(
            &identity_path,
            &format!(
                "{}\n",
                serde_json::json!({
                    "hook_event_name": "UserPromptSubmit",
                    "session_id": "ours-session",
                    "transcript_path": ours_path.display().to_string(),
                })
            ),
        );

        thread::sleep(Duration::from_millis(400));
        append_markers(&ours_path, &["OURS-SECOND-LINE"]);
        handle.signal_drain();
        drop(handle);

        let text = forwarded(&buf);
        assert!(
            text.contains("OURS-FIRST-LINE") && text.contains("OURS-SECOND-LINE"),
            "identity-named transcript was not forwarded whole (pre-bind bytes \
             included); got:\n{text}"
        );
        assert!(
            !text.contains("SIBLING-NEWEST"),
            "reader forwarded the newest-mtime candidate instead of the \
             identity payload's exact transcript_path; got:\n{text}"
        );
    }

    // ── claudepr-33fdf4ed: Phase-2 first-output credit ─────────────────────────

    /// The watchdog's first-output flag must be credited exactly when the
    /// reader forwards its FIRST transcript line — that credit is what makes
    /// the stream-json deadline a first-output deadline instead of the
    /// unconditional session cap it was while nothing could set the flag.
    #[test]
    fn first_forwarded_line_credits_watchdog_flag() {
        use std::sync::atomic::Ordering;

        let dir = TempDir::new().unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let (identity_path, projects_dir, buf, handle) =
            spawn_bound_with_flag(&dir, Some(Arc::clone(&flag)));

        assert!(
            !flag.load(Ordering::SeqCst),
            "flag must start unset before any output exists"
        );

        // Transcript content exists, but nothing is bound yet: nothing
        // forwarded, so nothing credited.
        let ours_path = projects_dir.join("ours-session.jsonl");
        append_markers(&ours_path, &["CREDIT-ME"]);
        thread::sleep(Duration::from_millis(200));
        assert!(
            !flag.load(Ordering::SeqCst),
            "flag was credited while the binding was still unresolved — \
             the deadline would be satisfied by output nobody forwarded"
        );

        // Identity resolves → the reader binds, forwards the line, and only
        // then credits the flag.
        write_identity(
            &identity_path,
            &format!(
                "{}\n",
                serde_json::json!({
                    "hook_event_name": "UserPromptSubmit",
                    "session_id": "ours-session",
                    "transcript_path": ours_path.display().to_string(),
                })
            ),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !flag.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "reader forwarded no first line within 5s of a resolved \
                 binding — the watchdog deadline would fire on a healthy run"
            );
            thread::sleep(Duration::from_millis(10));
        }
        handle.signal_drain();
        drop(handle);
        assert!(
            forwarded(&buf).contains("CREDIT-ME"),
            "the credit fired without the line actually being forwarded"
        );
    }

    /// The flag must be handed over untouched: a reader that never forwards
    /// (unresolved binding, no drain) must leave it unset, or the Phase-2
    /// deadline would be disarmed for a run that produced nothing.
    #[test]
    fn first_output_flag_stays_unset_while_nothing_is_forwarded() {
        use std::sync::atomic::Ordering;

        let dir = TempDir::new().unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let (_identity_path, projects_dir, buf, handle) =
            spawn_bound_with_flag(&dir, Some(Arc::clone(&flag)));

        // Two new candidates: attribution is refused, nothing is ever
        // forwarded, so the flag must stay unset.
        append_markers(&projects_dir.join("a.jsonl"), &["A"]);
        append_markers(&projects_dir.join("b.jsonl"), &["B"]);
        thread::sleep(IDENTITY_GRACE + Duration::from_millis(150));

        assert!(
            !flag.load(Ordering::SeqCst),
            "flag credited with zero forwarded output"
        );
        assert!(
            forwarded(&buf).trim().is_empty(),
            "reader forwarded an ambiguous candidate"
        );
        drop(handle);
        assert!(
            !flag.load(Ordering::SeqCst),
            "flag credited on exit without any forwarded line"
        );
    }

    /// `resolve_identity_binding` field ladder: explicit `transcript_path`
    /// preferred, `session_id` joined into the projects dir as fallback, and
    /// every unusable shape resolves to `None` (the caller keeps polling).
    #[test]
    fn resolve_identity_binding_prefers_explicit_path_then_session_id() {
        let dir = TempDir::new().unwrap();
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let identity_path = dir.path().join("session-identity.json");

        // Absent, empty, garbage, and field-less payloads all stay unresolved.
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            None
        );
        std::fs::write(&identity_path, "").unwrap();
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            None
        );
        std::fs::write(&identity_path, "{{{ nope").unwrap();
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            None
        );
        std::fs::write(&identity_path, r#"{"cwd":"/tmp"}"#).unwrap();
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            None
        );

        // Both fields: the explicit transcript_path wins.
        let explicit = "/tmp/explicit/transcript.jsonl";
        std::fs::write(
            &identity_path,
            serde_json::json!({
                "session_id": "sid-1",
                "transcript_path": explicit,
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            Some(PathBuf::from(explicit))
        );

        // Only session_id: joined into the reader's projects dir.
        std::fs::write(
            &identity_path,
            serde_json::json!({ "session_id": "sid-2" }).to_string(),
        )
        .unwrap();
        assert_eq!(
            resolve_identity_binding(&identity_path, &projects_dir),
            Some(projects_dir.join("sid-2.jsonl"))
        );
    }
}
