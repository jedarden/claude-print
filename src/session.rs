use crate::emitter;
use crate::error::{Error, Result};
use crate::event_loop::{EventLoop, ExitReason};
use crate::hook::HookInstaller;
use crate::poller::{open_fifo_nonblock, parse_stop_payload, resolve_stop_info};
use crate::pty::PtySpawner;
use crate::startup::{StartupAction, StartupSeq};
use crate::terminal::TerminalEmu;
use crate::transcript::{read_transcript, TranscriptResult};
use crate::watchdog::{TimeoutType, Watchdog, WatchdogConfig};
use nix::sys::signal::{self, SigHandler};
use nix::sys::wait::waitpid;
use std::ffi::{CString, OsString};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Global storage for the temp dir path that needs cleanup.
///
/// This is stored globally because `process::exit()` in main.rs bypasses
/// destructors, so we need to clean up explicitly before exit.
static TEMP_DIR_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Flag to track if cleanup has already been performed (prevents double cleanup).
static CLEANUP_PERFORMED: AtomicBool = AtomicBool::new(false);

/// Result of a Claude Code session.
#[derive(Debug)]
pub struct SessionResult {
    /// The parsed transcript result.
    pub transcript: TranscriptResult,
    /// Claude Code version string.
    pub claude_version: String,
    /// Session duration in milliseconds.
    pub duration_ms: u64,
    /// Path to the transcript file (for stream-json replay).
    pub transcript_path: std::path::PathBuf,
    /// Stream-json reader handle (only set when output_format is stream-json).
    pub stream_json_handle: Option<emitter::StreamJsonHandle>,
}

/// Headless-launch safety knobs (bf-uj0).
///
/// The PTY keyword scanner in [`crate::startup::StartupSeq`] dismisses the trust
/// dialog heuristically, but two startup blockers defeat it: a one-time folder
/// trust prompt for a never-trusted cwd, and an MCP server that hangs on connect.
/// These knobs remove both blocking paths *before* the child is spawned, so
/// headless runs can't wedge on an interactive prompt the scanner can't see.
///
/// All three default off — claude-print never mutates `~/.claude.json` or
/// overrides MCP config unless the caller explicitly asks. See [`Cli`] flag docs
/// for the per-knob rationale.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// MCP configs (path or inline JSON) to load. When non-empty, the child is
    /// launched with `--strict-mcp-config` plus `--mcp-config <entry>` for each
    /// entry, so ONLY the named configs load — inherited/project/global MCP
    /// servers that can hang on connect cannot wedge startup.
    pub mcp_configs: Vec<String>,
    /// Write `hasTrustDialogAccepted: true` for the cwd into `~/.claude.json`
    /// before spawning the child, so the one-time trust dialog can't stall an
    /// untrusted working dir.
    pub pretrust_cwd: bool,
    /// Capture the child's raw PTY output (combined stdout+stderr) and dump it
    /// to claude-print's stderr when startup is slow or stalls — surfaces
    /// MCP/init wedges for diagnosis.
    pub show_child_stderr: bool,
}

/// Bounded ring buffer of the child's raw PTY output (bf-uj0).
///
/// The child runs under a PTY, so this captures its combined stdout+stderr.
/// Kept to a fixed tail (`cap`) so a wedged-but-chatty child can't grow memory
/// unbounded; the tail is what matters for diagnosing a stall. A no-op until
/// `dump` is called on a slow/stall exit path, and only when the caller opted
/// in via `--show-child-stderr`.
struct ChildCapture {
    enabled: bool,
    buf: Vec<u8>,
    cap: usize,
}

impl ChildCapture {
    /// Maximum tail size kept, in bytes. ~64 KiB is enough to capture the
    /// failing region of a startup wedge without holding the whole session.
    const CAP: usize = 64 * 1024;

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            buf: Vec::new(),
            cap: Self::CAP,
        }
    }

    /// Append a PTY chunk, dropping the oldest bytes once `cap` is exceeded.
    fn feed(&mut self, chunk: &[u8]) {
        if !self.enabled || chunk.is_empty() {
            return;
        }
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.cap {
            let drop_n = self.buf.len() - self.cap;
            self.buf.drain(0..drop_n);
        }
    }

    /// Write the captured tail to claude-print's stderr with a bounding header.
    /// No-op when disabled or empty.
    fn dump(&self, reason: &str) {
        let mut stderr = std::io::stderr().lock();
        self.dump_to(reason, &mut stderr);
    }

    /// Core dump logic, separated from [`dump`] so tests can capture the
    /// rendered output into a buffer instead of redirecting process stderr.
    /// No-op when disabled or empty. Writes a bounding header, the raw tail,
    /// a trailing newline if the tail lacks one, and an end marker.
    fn dump_to(&self, reason: &str, w: &mut impl std::io::Write) {
        if !self.enabled || self.buf.is_empty() {
            return;
        }
        let _ = writeln!(
            w,
            "claude-print: ----- child PTY output ({}, {} bytes) -----",
            reason,
            self.buf.len()
        );
        let _ = w.write_all(&self.buf);
        if self.buf.last() != Some(&b'\n') {
            let _ = w.write_all(b"\n");
        }
        let _ = writeln!(w, "claude-print: ----- end child output -----");
    }
}

/// Guard that ensures temp dir cleanup on all exit paths.
///
/// This guard calls `installer.cleanup()` when dropped, ensuring that
/// temporary directories and FIFOs are removed even on error, timeout,
/// or signal interruption.
struct CleanupGuard<'a>(&'a HookInstaller);

impl<'a> Drop for CleanupGuard<'a> {
    fn drop(&mut self) {
        self.0.cleanup();
    }
}

/// Clean up the temp directory stored in the global variable.
///
/// This function is called before `process::exit()` to ensure cleanup
/// happens even when destructors are bypassed. It's idempotent - calling
/// it multiple times is safe.
pub fn cleanup_temp_dir() {
    // Use atomic swap to ensure we only cleanup once, even if called
    // from multiple threads or from atexit handler after explicit cleanup.
    if CLEANUP_PERFORMED.swap(true, Ordering::SeqCst) {
        // Already cleaned up
        return;
    }

    if let Some(path) = TEMP_DIR_PATH.get() {
        // Remove the FIFO first (it may have different permissions)
        // The FIFO must be removed before the directory can be deleted.
        let fifo_path = path.join("stop.fifo");
        for fifo_attempt in 0..3 {
            let result = std::fs::remove_file(&fifo_path);
            if result.is_ok() {
                break; // FIFO successfully removed
            }
            // If this is not the last attempt, wait a bit before retrying
            if fifo_attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // Ignore FIFO removal errors

        // Remove the entire temp directory with retry logic
        // This helps handle cases where files are temporarily locked
        for attempt in 0..3 {
            let result = std::fs::remove_dir_all(path);
            if result.is_ok() {
                break; // Successfully removed
            }
            // If this is not the last attempt, wait a bit before retrying
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        // Ignore final error - we've done our best
    }
}

/// Register cleanup as an atexit handler.
///
/// This ensures cleanup happens even on external signals that trigger
/// the default Rust handler (which calls process::exit() without running
/// destructors). The atexit handler is called by the C runtime before
/// process exit in all cases.
pub fn register_cleanup_handler() {
    extern "C" fn cleanup_atexit() {
        cleanup_temp_dir();
    }

    // Safety: cleanup_atexit only performs idempotent cleanup and is async-signal-safe.
    unsafe {
        libc::atexit(cleanup_atexit);
    }
}

/// Session orchestrator.
///
/// Manages the full lifecycle of a Claude Code PTY session.
pub struct Session;

impl Session {
    /// Run a Claude Code session.
    ///
    /// # Arguments
    ///
    /// * `claude_bin` - Path to the Claude Code binary.
    /// * `claude_args` - Flags to forward to Claude Code.
    /// * `prompt` - User prompt bytes to inject.
    /// * `timeout_secs` - Optional overall timeout in seconds.
    /// * `first_output_timeout_secs` - Optional first-output timeout in seconds.
    /// * `stream_json_timeout_secs` - Optional stream-json first-output timeout in seconds.
    /// * `stop_hook_timeout_secs` - Optional Stop hook watchdog timeout in seconds.
    /// * `output_format` - Output format (text, json, or stream-json).
    /// * `launch` - Headless-launch safety knobs (pre-trust cwd, bound MCP, stderr surfacing).
    ///
    /// # Returns
    ///
    /// Returns a `SessionResult` containing the transcript, Claude version, duration, and stream-json handle.
    ///
    /// # Errors
    ///
    /// Returns `Error::NoResponse` if the child exits without sending a Stop payload.
    /// Returns `Error::Timeout` if the timeout expires (no output or overall timeout).
    /// Returns `Error::Interrupted` if a SIGINT is received.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        claude_bin: &Path,
        claude_args: &[OsString],
        prompt: Vec<u8>,
        timeout_secs: Option<u64>,
        first_output_timeout_secs: Option<u64>,
        stream_json_timeout_secs: Option<u64>,
        stop_hook_timeout_secs: Option<u64>,
        output_format: crate::cli::OutputFormat,
        launch: &LaunchOptions,
    ) -> Result<SessionResult> {
        // Use a catch_unwind to ensure cleanup happens even on panics
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::run_inner(
                claude_bin,
                claude_args,
                prompt,
                timeout_secs,
                first_output_timeout_secs,
                stream_json_timeout_secs,
                stop_hook_timeout_secs,
                output_format,
                launch,
            )
        }));

        match result {
            Ok(inner_result) => inner_result,
            Err(_) => {
                // Panic occurred - cleanup already handled by CleanupGuard
                Err(Error::Internal(anyhow::anyhow!("Session panicked")))
            }
        }
    }

    /// Inner implementation of Session::run.
    ///
    /// This is separated from `run` to allow panic handling via catch_unwind
    /// while still ensuring cleanup happens through the CleanupGuard.
    #[allow(clippy::too_many_arguments)]
    fn run_inner(
        claude_bin: &Path,
        claude_args: &[OsString],
        prompt: Vec<u8>,
        timeout_secs: Option<u64>,
        first_output_timeout_secs: Option<u64>,
        stream_json_timeout_secs: Option<u64>,
        stop_hook_timeout_secs: Option<u64>,
        output_format: crate::cli::OutputFormat,
        launch: &LaunchOptions,
    ) -> Result<SessionResult> {
        let start_time = Instant::now();

        // 1. Install hook files (temp dir, hook.sh, stop.fifo).
        let installer = HookInstaller::new()?;

        // Store temp dir path globally for cleanup before process::exit()
        let _ = TEMP_DIR_PATH.set(installer.dir_path().to_path_buf());

        // 1a. Set up cleanup guard to ensure temp dir is removed on all exit paths
        let _cleanup_guard = CleanupGuard(&installer);

        // 2. Resolve Claude Code version.
        let claude_version = Self::resolve_claude_version(claude_bin)?;

        // 2a. Pre-grant folder trust for the cwd before spawning (bf-uj0). Claude
        // Code reads trust only from ~/.claude.json, never from --settings, so the
        // PTY keyword scanner is the only other line of defense — and it can miss
        // the one-time dialog for a never-trusted cwd. Doing this up front removes
        // the stall at the source. No-op unless --pretrust-cwd was passed.
        if launch.pretrust_cwd {
            pretrust_cwd()?;
        }

        // 3. Build child argv.
        let cmd = CString::new(claude_bin.to_string_lossy().as_bytes())
            .map_err(|e| Error::Internal(anyhow::anyhow!("claude_bin path invalid: {e}")))?;
        let mut args: Vec<CString> =
            Vec::with_capacity(claude_args.len() + 4 + 2 * launch.mcp_configs.len());
        args.push(CString::new("--dangerously-skip-permissions").unwrap());
        args.push(
            CString::new(format!(
                "--settings={}",
                installer.settings_path.to_string_lossy()
            ))
            .map_err(|e| Error::Internal(anyhow::anyhow!("settings path invalid: {e}")))?,
        );
        // Prevent global settings inheritance - the temp settings.json contains only the Stop hook
        // and inheriting global hooks (SessionStart, etc.) can cause the child to hang at startup.
        args.push(CString::new("--setting-sources=").unwrap());

        // bf-uj0: bound MCP init. When the caller names MCP configs, launch the
        // child in strict mode so ONLY those load — inherited/project/global MCP
        // servers that can hang on connect (a startup-wedge trigger from the
        // bf-2u1 investigation) are ignored entirely. Each entry is emitted as its
        // own `--mcp-config <entry>` pair: unambiguous regardless of what flags
        // follow, and tolerant of either inline-JSON or file-path values.
        if !launch.mcp_configs.is_empty() {
            args.push(CString::new("--strict-mcp-config").unwrap());
            for cfg in &launch.mcp_configs {
                args.push(CString::new("--mcp-config").unwrap());
                args.push(CString::new(cfg.as_str()).map_err(|e| {
                    Error::Internal(anyhow::anyhow!("mcp-config value invalid: {e}"))
                })?);
            }
        }

        for arg in claude_args {
            let arg_str = arg.to_string_lossy().to_string();
            args.push(
                CString::new(arg_str)
                    .map_err(|e| Error::Internal(anyhow::anyhow!("claude arg invalid: {e}")))?,
            );
        }

        // 4. Self-pipe for SIGINT.
        let (self_pipe_read, self_pipe_write) = nix::unistd::pipe()
            .map_err(|e| Error::Internal(anyhow::anyhow!("pipe() failed: {e}")))?;
        unsafe {
            let write_ptr = &raw mut SELF_PIPE_WRITE;
            *write_ptr = Some(self_pipe_write.try_clone().unwrap());
            signal::signal(signal::Signal::SIGINT, SigHandler::Handler(sigint_handler))
                .map_err(|e| Error::SignalHandlerFailed(format!("SIGINT: {e}")))?;
            signal::signal(
                signal::Signal::SIGTERM,
                SigHandler::Handler(sigterm_handler),
            )
            .map_err(|e| Error::SignalHandlerFailed(format!("SIGTERM: {e}")))?;
        }

        // Restore default signal handlers on drop.
        let _signal_guard = SignalGuard;

        // 5. Spawn PTY child.
        let spawner = PtySpawner::spawn(&cmd, &args)?;

        // 5a. Set up watchdog timeout handling.
        // We have four timeouts:
        // 1. PTY first-output timeout: if child emits no PTY data within N seconds (default 90s)
        // 2. Stream-json first-output timeout: if child emits no stream-json events within N seconds (default 90s)
        // 3. Overall timeout: if session exceeds overall deadline (default from CLI, 3600s)
        // 4. Stop hook watchdog timeout: if Stop hook doesn't fire within N seconds after prompt injection (default 120s)
        let watchdog_config = WatchdogConfig::new(
            first_output_timeout_secs,
            stream_json_timeout_secs.or(first_output_timeout_secs),
            timeout_secs,
            stop_hook_timeout_secs,
        );

        // Get temp directory path for stream-json monitoring
        // The watchdog will monitor <temp_dir>/transcript.jsonl for stream-json output
        let temp_dir_path = installer.dir_path().to_path_buf();

        // Get the raw fd for the self-pipe write end for the watchdog to signal timeout
        let watchdog_self_pipe_fd = Some(self_pipe_write.as_raw_fd());

        let watchdog = Watchdog::new(
            watchdog_config,
            spawner.child_pid,
            Some(temp_dir_path),
            watchdog_self_pipe_fd,
        );

        let watchdog_state = watchdog.state();

        // Spawn the watchdog timeout thread
        let _timeout_thread = watchdog.spawn_timeout_thread();

        // 6. Create event loop.
        let mut event_loop = EventLoop::new(spawner.master.as_raw_fd(), self_pipe_read.as_raw_fd());

        // 7. Create terminal emulator.
        let mut terminal = TerminalEmu::new(24, 80);

        // 8. Create startup sequence.
        let mut startup = StartupSeq::new(prompt);

        // 9. Open the FIFO before the event loop (so Stop hook can fire during the session).
        // Both the read-end and keeper write-end must be kept alive for the full duration of the
        // event loop: read_fd because the event loop polls its raw fd, keeper because without it
        // the hook's `cat > fifo` would get ENXIO when it tries to open the write-end.
        let (_fifo_read, _fifo_keeper) = match open_fifo_nonblock(&installer.fifo_path) {
            Ok((read_fd, keeper)) => {
                event_loop.add_fifo_fd(read_fd.as_raw_fd());
                (Some(read_fd), Some(keeper))
            }
            Err(e) => {
                eprintln!("warning: failed to open FIFO, continuing without Stop detection: {e}");
                (None, None)
            }
        };

        // 12. Prepare stream-json reader (will be spawned at PROMPT_INJECTED).
        //
        // Reader-thread cleanup is RAII: StreamJsonHandle::Drop disconnects the
        // drain channel and joins the reader, so EVERY return path below —
        // success, timeout, SIGINT/SIGTERM, child-exit, and the `?` propagations
        // from parse_stop_payload / read_transcript — joins the reader before
        // this function returns (plan invariant INV-8). Only the normal Stop
        // (success) path calls signal_drain() first; all error paths drop the
        // handle without signaling so the reader exits immediately.
        let temp_dir_path = installer.dir_path().to_path_buf();
        let transcript_path = temp_dir_path.join("transcript.jsonl");
        let mut stream_json_handle: Option<emitter::StreamJsonHandle> = None;
        let stream_json_spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // 12. Run the event loop.
        let master_fd = spawner.master.as_raw_fd();
        let watchdog_state_clone = watchdog_state.clone();
        let mut last_phase = startup.phase().clone();
        let stream_json_spawned_clone = stream_json_spawned.clone();
        // bf-uj0: capture the child's raw PTY output so --show-child-stderr can
        // surface it on a slow/stall exit. A no-op (empty buffer) until dumped.
        let mut child_capture = ChildCapture::new(launch.show_child_stderr);

        let exit_reason = event_loop.run(|chunk| {
            // Empty chunk = timer tick from the event loop (poll timeout with no data).
            // Only feed real data to the terminal emulator and startup sequence.
            if !chunk.is_empty() {
                // Mark that we've received first output from the child (PTY output)
                watchdog_state_clone.mark_pty_output();
                // Capture raw child output for --show-child-stderr diagnosis.
                child_capture.feed(chunk);
                // Feed chunk to terminal emulator.
                let probe_responses = terminal.feed(chunk);

                // Write probe responses to master.
                if !probe_responses.is_empty() {
                    unsafe {
                        libc::write(
                            master_fd,
                            probe_responses.as_ptr() as *const libc::c_void,
                            probe_responses.len(),
                        );
                    }
                }
            }

            // Feed chunk to startup sequence (skip empty ticks — feed() updates
            // last_output_at which would reset the idle timer).
            let action = if !chunk.is_empty() {
                startup.feed(chunk)
            } else {
                StartupAction::None
            };

            // Handle startup actions.
            match &action {
                StartupAction::Write(bytes) => unsafe {
                    libc::write(
                        master_fd,
                        bytes.as_ptr() as *const libc::c_void,
                        bytes.len(),
                    );
                },
                StartupAction::HardTimeout => {
                    // Handled after event loop exits.
                }
                StartupAction::None => {}
            }

            // Poll timers for startup sequence.
            let timer_action = startup.poll_timers();

            // Check if phase changed to PromptInjected and notify watchdog
            let current_phase = startup.phase();
            if last_phase != *current_phase && current_phase.is_prompt_injected() {
                watchdog_state_clone.mark_prompt_injected();

                // Spawn stream-json reader at PROMPT_INJECTED for stream-json output
                if matches!(output_format, crate::cli::OutputFormat::StreamJson) {
                    // Calculate byte offset: current transcript file size, or 0 if not exists
                    let start_offset = std::fs::metadata(&transcript_path)
                        .map(|m| m.len())
                        .unwrap_or(0);

                    stream_json_handle = Some(emitter::spawn_stream_json_reader(
                        transcript_path.clone(),
                        start_offset,
                    ));
                    stream_json_spawned_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            last_phase = current_phase.clone();

            match &timer_action {
                StartupAction::Write(bytes) => unsafe {
                    libc::write(
                        master_fd,
                        bytes.as_ptr() as *const libc::c_void,
                        bytes.len(),
                    );
                },
                StartupAction::HardTimeout => {
                    // Handled after event loop exits.
                }
                StartupAction::None => {}
            }
        })?;

        // 13. Check if watchdog timeout fired.
        if watchdog_state.has_timeout_fired() {
            let timeout_type = watchdog_state
                .get_timeout_type()
                .unwrap_or(TimeoutType::OverallTimeout);
            let timeout_msg = timeout_type.description();

            // Write diagnostic to stderr
            eprintln!("claude-print: {}", timeout_msg);
            eprintln!(
                "claude-print: sending SIGTERM to child pid {}",
                spawner.child_pid
            );

            // bf-uj0: a watchdog timeout means startup (or the session) stalled;
            // surface what the child emitted so MCP/init wedges are diagnosable.
            child_capture.dump(timeout_msg);

            kill_child(spawner.child_pid);

            // INV-8: Timeout path — drop the reader WITHOUT a drain signal (the
            // sender is dropped, not signaled). StreamJsonHandle::Drop disconnects
            // the channel so the reader exits immediately, then joins the thread
            // before we return.
            drop(stream_json_handle);

            return Err(Error::Timeout(timeout_msg.to_string()));
        }

        // 14. Handle exit reason.
        // bf-uj0: if the prompt was never injected, startup stalled before the
        // session even began — surface the child output for diagnosis on the
        // non-success arms below.
        let prompt_injected = startup.phase().is_prompt_injected();
        match exit_reason {
            ExitReason::FifoPayload(payload) => {
                // Parse stop payload. On error, `?` returns and Drop joins the
                // reader without draining (INV-8, exit-immediately on error).
                let stop_payload = parse_stop_payload(&payload)?;
                let stop_info = resolve_stop_info(stop_payload);

                // Read transcript. On error, `?` returns and Drop joins the
                // reader without draining (INV-8, exit-immediately on error).
                let transcript_path = stop_info.transcript_path.as_ref();
                let transcript = if let Some(path) = transcript_path {
                    let t = read_transcript(path, stop_info.last_assistant_message.as_deref())?;
                    // bf-416c: Claude Code's own transcript result event reported
                    // is_error:true (rate limit, tool failure, any assistant-side
                    // error). This is a COMPLETED turn that the assistant itself
                    // flagged as failed — distinct from a claude-print Setup error.
                    // Surface it as exit-1 AssistantError so callers that gate on
                    // exit code / is_error (NEEDLE's output_transform) don't
                    // silently treat a failed turn as success. The reader handle
                    // is dropped (joined without draining) on this return — same
                    // INV-8 exit-immediately pattern as the `?` propagations above.
                    if t.is_error {
                        return Err(Error::AssistantError(t.text));
                    }
                    t
                } else {
                    // No transcript path: error path — Drop joins without draining.
                    return Err(Error::Internal(anyhow::anyhow!(
                        "Stop payload contained no transcript path and could not derive one"
                    )));
                };

                // Wait for child to exit.
                kill_child(spawner.child_pid);

                let transcript_path = stop_info
                    .transcript_path
                    .clone()
                    .unwrap_or_else(|| std::path::PathBuf::from("transcript.jsonl"));

                // Normal Stop transition: signal the reader to drain its
                // remaining transcript lines to stdout, then drop it so Drop
                // disconnects the channel and joins (draining completes inside
                // the join before we return). INV-8.
                if let Some(handle) = stream_json_handle.as_ref() {
                    handle.signal_drain();
                }
                drop(stream_json_handle);

                Ok(SessionResult {
                    transcript,
                    claude_version,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                    transcript_path,
                    stream_json_handle: None, // reader drained + joined via Drop above
                })
            }
            ExitReason::ChildExited => {
                // Child exited without Stop hook.
                let _ = waitpid(spawner.child_pid, None);
                // bf-uj0: if the prompt was never injected, the child died during
                // startup (bad args, init crash) — surface its output.
                if !prompt_injected {
                    child_capture.dump("child exited before prompt was injected");
                }
                // Drop joins the reader without draining (INV-8, exit-immediately).
                drop(stream_json_handle);
                Err(Error::Internal(anyhow::anyhow!(
                    "Child exited without sending Stop payload"
                )))
            }
            ExitReason::Interrupted => {
                kill_child(spawner.child_pid);
                // bf-uj0: surface output if interrupted before injection (likely a
                // user-noticed stall).
                if !prompt_injected {
                    child_capture.dump("interrupted before prompt was injected");
                }
                // SIGINT/SIGTERM path: the sender is dropped (not signaled), so
                // the reader exits immediately; Drop joins it before we return (INV-8).
                drop(stream_json_handle);
                Err(Error::Interrupted("interrupted by signal".to_string()))
            }
        }
    }

    /// Resolve Claude Code version string.
    ///
    /// Runs `claude --version` and captures the first line of output.
    fn resolve_claude_version(claude_bin: &Path) -> Result<String> {
        let output = Command::new(claude_bin)
            .arg("--version")
            .output()
            .map_err(|e| Error::Internal(anyhow::anyhow!("failed to run claude --version: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", stdout, stderr);
        let first_line = combined.lines().next().ok_or_else(|| {
            Error::Internal(anyhow::anyhow!("claude --version produced no output"))
        })?;

        Ok(first_line.trim().to_string())
    }
}

/// Send SIGTERM to `pid`, wait up to 2 seconds, then SIGKILL if still alive.
fn kill_child(pid: nix::unistd::Pid) {
    use nix::sys::wait::WaitPidFlag;
    use nix::sys::wait::WaitStatus;

    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match nix::sys::wait::waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if Instant::now() >= deadline {
                    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                    let _ = nix::sys::wait::waitpid(pid, None);
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            _ => return,
        }
    }
}

/// Pre-grant folder trust for the current working dir by writing
/// `hasTrustDialogAccepted: true` into `~/.claude.json` (bf-uj0).
///
/// Claude Code reads workspace trust only from this file (never from `--settings`),
/// keyed under `projects["<abs-cwd>"]`. Setting the flag before spawn removes the
/// one-time trust dialog as a startup blocker for an untrusted cwd — the PTY
/// keyword scanner can miss it, and a missed dialog stalls the session forever.
///
/// Safety: the existing file is parsed and only the trust field is modified, then
/// written back atomically (sibling tmp file + rename) preserving its mode. If the
/// file exists but is not a valid JSON object, it is left **untouched** (the trust
/// scanner remains the fallback) — clobbering the user's config would be far worse
/// than a possible stall.
fn pretrust_cwd() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Internal(anyhow::anyhow!("pretrust cwd: {e}")))?;
    let home = std::env::var("HOME")
        .map_err(|_| Error::Config("HOME environment variable not set".to_string()))?;
    let claude_json = PathBuf::from(&home).join(".claude.json");
    pretrust_cwd_at(&claude_json, cwd.to_string_lossy().as_ref())
}

/// Inner file-mutation logic for [`pretrust_cwd`], separated so it can be
/// exercised against a temp path without touching the real `$HOME` or racing
/// other tests on a process-global env var.
///
/// Sets `projects[cwd_abs].hasTrustDialogAccepted = true` in the JSON object at
/// `claude_json`, creating the file if absent. Safety: if the file exists but is
/// not a valid JSON object, it is left **untouched** (the trust scanner remains
/// the fallback) — clobbering the user's config would be far worse than a
/// possible stall.
fn pretrust_cwd_at(claude_json: &Path, cwd_abs: &str) -> Result<()> {
    // Read existing content + mode. On a parse error of an *existing* file, do
    // NOT rewrite — clobbering the user's config is worse than a possible stall.
    let (mut root, existing_mode) = match std::fs::read_to_string(claude_json) {
        Ok(s) => {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(claude_json)
                .ok()
                .map(|m| m.permissions().mode());
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(v) if v.is_object() => (v, mode),
                Ok(_) => {
                    eprintln!(
                        "claude-print: warning: ~/.claude.json is not a JSON object; leaving it untouched (trust scanner remains active)"
                    );
                    return Ok(());
                }
                Err(e) => {
                    eprintln!(
                        "claude-print: warning: ~/.claude.json is unreadable ({e}); leaving it untouched (trust scanner remains active)"
                    );
                    return Ok(());
                }
            }
        }
        Err(_) => (serde_json::json!({}), None),
    };

    // projects[cwd].hasTrustDialogAccepted = true
    let key = cwd_abs.to_owned();
    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| Error::Internal(anyhow::anyhow!("~/.claude.json root is not an object")))?;
    let projects = root_obj
        .entry("projects".to_string())
        .or_insert(serde_json::json!({}));
    let proj = projects.as_object_mut().ok_or_else(|| {
        Error::Internal(anyhow::anyhow!("~/.claude.json projects is not an object"))
    })?;
    let entry = proj.entry(key).or_insert(serde_json::json!({}));
    let entry_obj = entry.as_object_mut().ok_or_else(|| {
        Error::Internal(anyhow::anyhow!(
            "~/.claude.json project entry is not an object"
        ))
    })?;
    entry_obj.insert(
        "hasTrustDialogAccepted".to_string(),
        serde_json::json!(true),
    );

    // Atomic write: sibling tmp file (same dir ⇒ same filesystem) + rename.
    let tmp_path = claude_json.with_file_name(format!(
        ".claude.json.tmp-claude-print-{}",
        std::process::id()
    ));
    let content = serde_json::to_string(&root)
        .map_err(|e| Error::Internal(anyhow::anyhow!("serialize ~/.claude.json: {e}")))?;
    std::fs::write(&tmp_path, content)
        .map_err(|e| Error::Internal(anyhow::anyhow!("write ~/.claude.json tmp: {e}")))?;

    // Preserve existing mode, or default to 0600 for a newly created file
    // (~/.claude.json holds auth/session state).
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = existing_mode.unwrap_or(0o600);
        if let Err(e) = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode)) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(Error::Internal(anyhow::anyhow!(
                "chmod ~/.claude.json tmp: {e}"
            )));
        }
    }

    std::fs::rename(&tmp_path, claude_json).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        Error::Internal(anyhow::anyhow!("rename ~/.claude.json: {e}"))
    })?;

    Ok(())
}

// Signal handler that writes to the self-pipe.
static mut SELF_PIPE_WRITE: Option<std::os::unix::io::OwnedFd> = None;

extern "C" fn sigint_handler(_: libc::c_int) {
    unsafe {
        let fd_ptr = &raw const SELF_PIPE_WRITE;
        let fd_option = &*fd_ptr;
        if let Some(fd) = fd_option {
            // Write one byte to the pipe (ignore errors).
            let byte: [u8; 1] = [1];
            let _ = nix::unistd::write(fd, &byte);
        }
    }
}

extern "C" fn sigterm_handler(_: libc::c_int) {
    unsafe {
        let fd_ptr = &raw const SELF_PIPE_WRITE;
        let fd_option = &*fd_ptr;
        if let Some(fd) = fd_option {
            // Write one byte to the pipe (ignore errors).
            let byte: [u8; 1] = [1];
            let _ = nix::unistd::write(fd, &byte);
        }
    }
}

/// Guard that restores default signal handlers on drop.
struct SignalGuard;

impl Drop for SignalGuard {
    fn drop(&mut self) {
        let _ = unsafe {
            signal::signal(signal::Signal::SIGINT, SigHandler::SigDfl)
                .and(signal::signal(signal::Signal::SIGTERM, SigHandler::SigDfl))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_resolve_claude_version_with_echo() {
        // Test with /bin/echo which is always available.
        let echo_path = Path::new("/bin/echo");
        if echo_path.exists() {
            let result = Session::resolve_claude_version(echo_path);
            // This will fail because echo doesn't output the right format,
            // but we're just testing that the function runs without panicking.
            assert!(result.is_ok() || result.is_err());
        }
    }

    #[test]
    fn test_resolve_claude_version_with_nonexistent_binary() {
        let nonexistent = Path::new("/nonexistent/binary/path");
        let result = Session::resolve_claude_version(nonexistent);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_resolution_with_mock_binary() {
        // Create a mock binary that outputs a version string
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mock_bin = temp_dir.path().join("mock-claude-version");

        // Resolve bash via PATH and use its absolute path in the shebang: a
        // hardcoded `#!/bin/bash` breaks on non-FHS systems (e.g. NixOS, where
        // /bin/bash does not exist and the kernel cannot exec the mock script).
        let bash = which::which("bash").expect("bash should be resolvable on PATH");
        let mock_script = format!(
            "#!{}\nif [[ \"$1\" == \"--version\" ]]; then\n    echo \"claude-print-mock-1.0.0\"\n    exit 0\nfi\nexit 1\n",
            bash.display(),
        );

        fs::write(&mock_bin, mock_script).unwrap();

        // Make it executable
        let mut perms = fs::metadata(&mock_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&mock_bin, perms).unwrap();

        // Test version resolution
        let result = Session::resolve_claude_version(&mock_bin);
        assert!(
            result.is_ok(),
            "Version resolution should succeed: {:?}",
            result
        );
        assert_eq!(result.unwrap(), "claude-print-mock-1.0.0");
    }

    #[test]
    fn test_session_result_struct_has_required_fields() {
        // This test verifies that SessionResult has the required fields
        // by checking that we can construct and access them
        use crate::transcript::{AggregatedUsage, TranscriptResult};

        let transcript = TranscriptResult {
            text: "test".to_string(),
            num_turns: 1,
            usage: AggregatedUsage::default(),
            is_error: false,
            session_id: Some("sess-123".to_string()),
            used_fallback: false,
        };

        let session_result = SessionResult {
            transcript,
            claude_version: "claude-1.0.0".to_string(),
            duration_ms: 1000,
            transcript_path: std::path::PathBuf::from("transcript.jsonl"),
            stream_json_handle: None,
        };

        assert_eq!(session_result.claude_version, "claude-1.0.0");
        assert_eq!(session_result.duration_ms, 1000);
        assert_eq!(session_result.transcript.text, "test");
        assert_eq!(
            session_result.transcript.session_id.as_deref(),
            Some("sess-123")
        );
        assert_eq!(
            session_result.transcript_path,
            std::path::PathBuf::from("transcript.jsonl")
        );
    }

    // ── ChildCapture (bf-uj0): --show-child-stderr ring buffer ────────────────

    #[test]
    fn child_capture_disabled_is_noop() {
        let mut c = ChildCapture::new(false);
        c.feed(b"some output");
        assert!(c.buf.is_empty(), "disabled capture must not accumulate");
    }

    #[test]
    fn child_capture_accumulates_when_enabled() {
        let mut c = ChildCapture::new(true);
        c.feed(b"hello ");
        c.feed(b"world");
        assert_eq!(c.buf, b"hello world");
    }

    #[test]
    fn child_capture_evicts_oldest_past_cap() {
        // Bounded tail: once cap is exceeded the oldest bytes drop so a chatty
        // but wedged child can't grow memory unbounded.
        let mut c = ChildCapture::new(true);
        c.cap = 10;
        c.feed(b"0123456789ABCDEF"); // 16 bytes → keep last 10
        assert_eq!(c.buf.len(), 10);
        assert_eq!(&*c.buf, b"6789ABCDEF");
    }

    #[test]
    fn child_capture_dump_to_noop_when_disabled() {
        let mut c = ChildCapture::new(false);
        c.buf.extend_from_slice(b"data");
        let mut out = Vec::new();
        c.dump_to("reason", &mut out);
        assert!(out.is_empty(), "disabled capture must dump nothing");
    }

    #[test]
    fn child_capture_dump_to_noop_when_empty() {
        let c = ChildCapture::new(true);
        let mut out = Vec::new();
        c.dump_to("reason", &mut out);
        assert!(out.is_empty(), "empty capture must dump nothing");
    }

    #[test]
    fn child_capture_dump_to_renders_header_bytes_and_trailing_newline() {
        let mut c = ChildCapture::new(true);
        c.buf.extend_from_slice(b"child stderr line"); // no trailing newline
        let mut out = Vec::new();
        c.dump_to("watchdog fired", &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("----- child PTY output (watchdog fired, 17 bytes) -----"));
        assert!(s.contains("child stderr line"));
        assert!(s.contains("----- end child output -----"));
        // Tail lacked a newline → one must be appended before the end marker.
        assert!(
            s.contains("child stderr line\n"),
            "trailing newline must be added when tail lacks one"
        );
    }

    #[test]
    fn child_capture_dump_to_no_extra_newline_when_tail_ends_in_newline() {
        let mut c = ChildCapture::new(true);
        c.buf.extend_from_slice(b"line\n");
        let mut out = Vec::new();
        c.dump_to("r", &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("line\n\n"), "must not double the trailing newline");
    }

    // ── pretrust_cwd_at (bf-uj0): ~/.claude.json trust pre-grant ──────────────

    /// Helper: read ~/.claude.json back as a JSON object (panics on failure).
    fn read_claude_json(path: &Path) -> serde_json::Value {
        let s = std::fs::read_to_string(path).expect("claude.json readable");
        serde_json::from_str(&s).expect("claude.json valid JSON object")
    }

    #[test]
    fn pretrust_creates_file_when_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        pretrust_cwd_at(&json, "/abs/cwd").unwrap();

        let v = read_claude_json(&json);
        assert_eq!(
            v["projects"]["/abs/cwd"]["hasTrustDialogAccepted"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn pretrust_sets_flag_preserving_existing_project_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        // Pre-existing project with unrelated state, and a sibling project.
        std::fs::write(
            &json,
            r#"{"projects":{"/abs/cwd":{"history":["a","b"]},"/other":{"x":1}}}"#,
        )
        .unwrap();

        pretrust_cwd_at(&json, "/abs/cwd").unwrap();

        let v = read_claude_json(&json);
        assert_eq!(
            v["projects"]["/abs/cwd"]["hasTrustDialogAccepted"],
            serde_json::json!(true)
        );
        // Existing fields must survive.
        assert_eq!(v["projects"]["/abs/cwd"]["history"][1], serde_json::json!("b"));
        assert_eq!(v["projects"]["/other"]["x"], serde_json::json!(1));
    }

    #[test]
    fn pretrust_preserves_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        std::fs::write(&json, r#"{"projects":{}}"#).unwrap();
        std::fs::set_permissions(&json, std::fs::Permissions::from_mode(0o644)).unwrap();

        pretrust_cwd_at(&json, "/abs/cwd").unwrap();

        let mode = std::fs::metadata(&json).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "existing mode must be preserved, not reset to 0600");
    }

    #[test]
    fn pretrust_new_file_gets_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        pretrust_cwd_at(&json, "/abs/cwd").unwrap();
        let mode = std::fs::metadata(&json).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "newly created ~/.claude.json must default to 0600 (holds auth state)"
        );
    }

    #[test]
    fn pretrust_leaves_invalid_json_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        let original = b"{ not valid json";
        std::fs::write(&json, original).unwrap();

        // Returns Ok (scanner remains the fallback) but must NOT clobber.
        pretrust_cwd_at(&json, "/abs/cwd").unwrap();

        assert_eq!(
            std::fs::read(&json).unwrap(),
            original,
            "unparseable ~/.claude.json must be left byte-for-byte untouched"
        );
        // No leftover tmp file in the dir.
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no tmp file must remain after the run"
        );
    }

    #[test]
    fn pretrust_leaves_non_object_root_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        let json = dir.path().join(".claude.json");
        // Valid JSON but an array, not an object.
        let original = b"[1, 2, 3]";
        std::fs::write(&json, original).unwrap();

        pretrust_cwd_at(&json, "/abs/cwd").unwrap();

        assert_eq!(
            std::fs::read(&json).unwrap(),
            original,
            "non-object root must be left untouched"
        );
    }
}
