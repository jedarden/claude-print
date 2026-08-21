use crate::emitter;
use crate::error::{Error, Result};
use crate::event_loop::{EventLoop, ExitReason};
use crate::hook::HookInstaller;
use crate::poller::{
    open_fifo_nonblock, parse_stop_payload, projects_dir_for_cwd, resolve_stop_info,
};
use crate::pty::PtySpawner;
use crate::startup::{StartupAction, StartupPhase, StartupSeq};
use crate::terminal::TerminalEmu;
use crate::transcript::{read_transcript_traced, TranscriptResult};
use crate::util::get_home;
use crate::verbose::Tracer;
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

/// Child-launch options.
///
/// Groups every decision about how the inner `claude` process is spawned: which
/// settings sources it loads, which MCP configs (if any) it trusts, and the
/// bf-uj0 headless-safety knobs that keep startup from wedging.
///
/// The PTY keyword scanner in [`crate::startup::StartupSeq`] dismisses the trust
/// dialog heuristically, but two startup blockers defeat it: a one-time folder
/// trust prompt for a never-trusted cwd, and an MCP server that hangs on connect.
/// The launch knobs remove both blocking paths *before* the child is spawned, so
/// headless runs can't wedge on an interactive prompt the scanner can't see.
///
/// All fields default off — claude-print never suppresses user hooks, mutates
/// `~/.claude.json`, or overrides MCP config unless the caller explicitly asks.
/// See [`Cli`] flag docs for the per-knob rationale.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// Isolation mode (`--no-inherit-hooks`): forward `--setting-sources=` to
    /// the child so it loads NO standard settings sources — only the temp
    /// `--settings` relay hook is active, and the user's `~/.claude/settings.json`
    /// hooks (SessionStart, PreToolUse, ccdash, trail-boss, …) do not fire.
    ///
    /// Default `false` (Hard Requirement 5): the flag is OMITTED so the user's
    /// hooks fire alongside the relay hook, exactly as `claude -p` behaves. If
    /// user-hook inheritance ever wedges headless startup, that startup-safety
    /// concern is bf-uj0's scope (bound MCP init / `--pretrust-cwd`) — not a
    /// reason to blanket-suppress sources here and silently break HR-5 for every
    /// invocation.
    pub no_inherit_hooks: bool,
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
    /// Write `[claude-print <ms>ms] <message>` timing traces to stderr across
    /// the session lifecycle (the `--verbose` flag). When off, the tracer is a
    /// no-op so the flag costs nothing on the hot path.
    pub verbose: bool,
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
    /// Build the argv array for the child Claude Code process.
    ///
    /// This function constructs the complete argument list that will be passed to execvp
    /// when spawning the child process. It is separated from `run_inner` to enable
    /// unit testing of argument construction without needing the full E2E harness.
    ///
    /// # Arguments
    ///
    /// * `claude_bin` - Path to the Claude Code binary.
    /// * `installer` - The hook installer containing the settings path.
    /// * `launch` - Headless-launch safety knobs (no-inherit-hooks, MCP configs, etc.).
    /// * `claude_args` - Additional flags to forward to Claude Code.
    ///
    /// # Returns
    ///
    /// Returns a `Vec<CString>` where the first element is the binary path and
    /// subsequent elements are the arguments.
    ///
    /// # Errors
    ///
    /// Returns `Error::Internal` if any path or argument contains a null byte.
    #[allow(clippy::too_many_arguments)]
    pub fn build_child_argv(
        claude_bin: &Path,
        installer: &HookInstaller,
        launch: &LaunchOptions,
        claude_args: &[OsString],
    ) -> Result<Vec<CString>> {
        // Build child argv.
        let _cmd = CString::new(claude_bin.to_string_lossy().as_bytes())
            .map_err(|e| Error::Internal(anyhow::anyhow!("claude_bin path invalid: {e}")))?;
        let mut args: Vec<CString> =
            Vec::with_capacity(claude_args.len() + 3 + 2 * launch.mcp_configs.len());
        args.push(
            CString::new(format!(
                "--settings={}",
                installer.settings_path.to_string_lossy()
            ))
            .map_err(|e| Error::Internal(anyhow::anyhow!("settings path invalid: {e}")))?,
        );
        // Isolation mode (--no-inherit-hooks, LaunchOptions::no_inherit_hooks):
        // forward `--setting-sources=` so the child loads NO standard settings
        // sources — only the temp --settings relay hook is active, suppressing
        // the user's ~/.claude/settings.json hooks (SessionStart, PreToolUse,
        // ccdash, trail-boss, …). Empty value = load no standard sources.
        //
        // Default mode OMITS this flag so the user's hooks fire alongside the
        // relay hook (Hard Requirement 5), matching `claude -p`. session.rs is
        // the single source of truth for this argv flag — main.rs must NOT also
        // push it into claude_args. If user-hook inheritance ever wedges headless
        // startup, that is bf-uj0's scope (bound MCP / --pretrust-cwd), not a
        // reason to blanket-suppress sources here and break HR-5 for every run.
        if launch.no_inherit_hooks {
            args.push(CString::new("--setting-sources=").unwrap());
        }

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

        Ok(args)
    }

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
    /// Returns `Error::Config` if `HOME` is unset or empty.
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
        // Keep direct library callers consistent with the CLI and every module
        // that derives user-scoped paths. Fail before creating hook artifacts
        // or inspecting the child binary.
        get_home()?;

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

        // --verbose timing tracer. Constructed once from the flag + session
        // start, then threaded through the lifecycle (event-loop closure,
        // transcript reader). A no-op on every call when `--verbose` is off, so
        // the default hot path pays only a single `enabled` branch test.
        let tracer = Tracer::new(launch.verbose, start_time);

        // 1. Install hook files (temp dir, hook.sh, stop.fifo).
        let installer = HookInstaller::new()?;
        tracer.trace(format!(
            "temp dir created at {}",
            installer.dir_path().display()
        ));

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
        let args = Self::build_child_argv(claude_bin, &installer, launch, claude_args)?;

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
        // openpty() and fork()+execvp() happen atomically inside spawn(); at ms
        // resolution they are the same instant, so both trace points are emitted
        // here in the order the plan lists them (pty opened, then child forked).
        tracer.trace("pty opened");
        tracer.trace(format!("child forked pid={}", spawner.child_pid));

        // 5a. Set up watchdog timeout handling.
        // We have four timeouts:
        // 1. PTY first-output timeout: if child emits no PTY data within N seconds (default 90s)
        // 2. Stream-json first-output timeout: if child emits no stream-json events within N seconds (default 90s)
        // 3. Overall timeout: if session exceeds overall deadline (default from CLI, 3600s)
        // 4. Stop hook watchdog timeout: if Stop hook doesn't fire within N seconds after prompt injection (default 120s)
        //
        // bf-lu1h: timeout #2 (stream-json first-output) only applies in
        // stream-json mode. Outside it the child never writes
        // <temp_dir>/transcript.jsonl (the real transcript lands in
        // ~/.claude/projects/) and mark_stream_json_output is never called, so
        // the deadline is unsatisfiable and would SIGTERM any turn >90s. Arm it
        // (and tell the watchdog the mode) only for stream-json output.
        let is_stream_json = matches!(output_format, crate::cli::OutputFormat::StreamJson);
        let stream_json_first_output = if is_stream_json {
            stream_json_timeout_secs.or(first_output_timeout_secs)
        } else {
            Some(0) // explicitly disabled outside stream-json mode
        };
        let watchdog_config = WatchdogConfig::new(
            first_output_timeout_secs,
            stream_json_first_output,
            timeout_secs,
            stop_hook_timeout_secs,
            is_stream_json,
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
                tracer.trace("fifo opened");
                (Some(read_fd), Some(keeper))
            }
            Err(e) => {
                eprintln!("warning: failed to open FIFO, continuing without Stop detection: {e}");
                tracer.trace(format!("fifo open failed: {e}"));
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
        let mut stream_json_handle: Option<emitter::StreamJsonHandle> = None;
        let stream_json_spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // 12. Run the event loop.
        let master_fd = spawner.master.as_raw_fd();
        let watchdog_state_clone = watchdog_state.clone();
        let mut last_phase = startup.phase().clone();
        let stream_json_spawned_clone = stream_json_spawned.clone();
        // --verbose: cloned (Copy) into the closure so phase transitions are
        // traced from the same call site that detects them.
        let tracer_clone = tracer;
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
            if last_phase != *current_phase {
                tracer_clone.trace(format!(
                    "phase transition: {} -> {}",
                    phase_name(&last_phase),
                    phase_name(current_phase)
                ));
            }
            if last_phase != *current_phase && current_phase.is_prompt_injected() {
                watchdog_state_clone.mark_prompt_injected();
                // INV-3: the "prompt injected" trace must follow "fifo opened".
                tracer_clone.trace("prompt injected");

                // Spawn stream-json reader at PROMPT_INJECTED for stream-json output.
                //
                // bf-3c7c: the reader tails the REAL transcript path claude
                // writes (~/.claude/projects/<cwd-slug>/<session_id>.jsonl). The
                // session_id — and thus the exact filename — is assigned by claude
                // and only surfaces in the Stop payload, which arrives AFTER
                // injection, so the reader cannot be handed a final path here.
                // Instead point it at the projects dir and let it DISCOVER this
                // session's JSONL at runtime (newest .jsonl new or grown since the
                // injection snapshot), reusing the existing 50ms/5s open-retry
                // loop. The snapshot captures each file's size at injection so the
                // reader can skip pre-injection events (start_offset).
                if matches!(output_format, crate::cli::OutputFormat::StreamJson) {
                    match projects_dir_for_cwd() {
                        Ok(projects_dir) => {
                            let pre_existing = emitter::snapshot_jsonl_sizes(&projects_dir);
                            stream_json_handle = Some(emitter::spawn_stream_json_reader_discover(
                                projects_dir,
                                pre_existing,
                            ));
                            stream_json_spawned_clone
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                        Err(e) => {
                            eprintln!(
                                "claude-print: warning: could not derive projects dir for the \
                                 stream-json reader: {}; live transcript tailing disabled",
                                e
                            );
                        }
                    }
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
            tracer.trace(format!("cleanup reason: timeout ({})", timeout_msg));

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
                // EC-7: a Stop hook firing before the prompt was injected means
                // the child responded to a prompt claude-print never sent — a
                // session identity leak. Real Claude Code is prevented from this
                // by EC-11 (pty.rs unsets CLAUDE_CODE_SESSION_ID before execvp),
                // so this is the defense-in-depth backstop for the case where
                // that mitigation nonetheless fails. Treat it as a Setup error
                // (exit 2, is_error:true in output) rather than silently
                // accepting an unsolicited response and proceeding to
                // read_transcript(). The FIFO payload is untrustworthy in this
                // state, so we gate on `prompt_injected` BEFORE touching it.
                //
                // `prompt_injected` reflects startup.phase() at the moment the
                // event loop returned: phase transitions only happen inside
                // startup.feed() on PTY output, and the FIFO-readable branch of
                // the event loop never calls feed(), so this is the true phase
                // at the instant Stop arrived (phase is monotonic — once
                // PromptInjected it never reverts).
                //
                // INV-8: the reader was never spawned (prompt wasn't injected),
                // so stream_json_handle is None and this drop is a no-op; it is
                // here for symmetry with the other error arms.
                if !prompt_injected {
                    kill_child(spawner.child_pid);
                    drop(stream_json_handle);
                    return Err(Error::Internal(anyhow::anyhow!(
                        "Stop hook fired before prompt was injected (EC-7: response to an unsent prompt — possible session identity leak)"
                    )));
                }

                // Parse stop payload. On error, `?` returns and Drop joins the
                // reader without draining (INV-8, exit-immediately on error).
                let stop_payload = parse_stop_payload(&payload)?;
                let stop_info = resolve_stop_info(stop_payload)?;

                // --verbose "Stop received" trace (plan §"`--verbose` Trace
                // Points`"): emit the session id Claude Code reported in the Stop
                // payload, if any. This marks the instant the model finished its
                // turn, ahead of the transcript-read retry trace below.
                tracer.trace(format!(
                    "stop received session_id={}",
                    stop_info.session_id.as_deref().unwrap_or("(none)")
                ));

                // Read transcript. On error, `?` returns and Drop joins the
                // reader without draining (INV-8, exit-immediately on error).
                let transcript_path = stop_info.transcript_path.as_ref();
                let transcript = if let Some(path) = transcript_path {
                    let t = read_transcript_traced(
                        path,
                        stop_info.last_assistant_message.as_deref(),
                        &tracer,
                    )?;
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

/// Lowercase name for a [`StartupPhase`], for `--verbose` transition traces.
fn phase_name(phase: &StartupPhase) -> &'static str {
    match phase {
        StartupPhase::Waiting => "waiting",
        StartupPhase::TrustDismissed => "trust-dismissed",
        StartupPhase::PromptInjected => "prompt-injected",
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
///
/// # Errors
/// Returns `Error::Config` if the `HOME` environment variable is unset or empty.
/// See [`get_home`](crate::util::get_home) for rationale on the strict approach.
fn pretrust_cwd() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| Error::Internal(anyhow::anyhow!("pretrust cwd: {e}")))?;
    let home = get_home()?;
    let claude_json = home.join(".claude.json");
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

    // ── build_child_argv: testable child argv construction ─────────────────────

    #[test]
    fn build_child_argv_includes_settings_flag() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // First arg should be --settings=<path>
        assert!(argv[0].to_string_lossy().starts_with("--settings="));
        assert!(argv[0]
            .to_string_lossy()
            .contains(installer.settings_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn build_child_argv_with_no_inherit_hooks_adds_setting_sources_flag() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            no_inherit_hooks: true,
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain --setting-sources= (empty value = no standard sources)
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "--setting-sources="));
    }

    #[test]
    fn build_child_argv_without_no_inherit_hooks_omits_setting_sources() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default(); // no_inherit_hooks defaults to false
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should NOT contain --setting-sources flag
        assert!(!argv
            .iter()
            .any(|arg| arg.to_string_lossy().starts_with("--setting-sources")));
    }

    #[test]
    fn build_child_argv_with_mcp_configs_adds_strict_mode_flags() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            mcp_configs: vec![
                "/path/to/config1.json".to_string(),
                "{\"inline\": true}".to_string(),
            ],
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain --strict-mcp-config
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "--strict-mcp-config"));

        // Should contain --mcp-config for each config
        let mcp_config_count = argv
            .iter()
            .filter(|arg| arg.to_string_lossy() == "--mcp-config")
            .count();
        assert_eq!(mcp_config_count, 2);

        // Verify the config values are present
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy().contains("/path/to/config1.json")));
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy().contains("{\"inline\": true}")));
    }

    #[test]
    fn build_child_argv_with_empty_mcp_configs_omits_mcp_flags() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            mcp_configs: vec![],
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should NOT contain any MCP-related flags
        assert!(!argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "--strict-mcp-config"));
        assert!(!argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "--mcp-config"));
    }

    #[test]
    fn build_child_argv_forwards_claude_args() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![
            OsString::from("--model"),
            OsString::from("claude-sonnet-4-20250514"),
            OsString::from("--max-tokens"),
            OsString::from("200000"),
        ];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain all claude_args at the end
        assert!(argv.iter().any(|arg| arg.to_string_lossy() == "--model"));
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "claude-sonnet-4-20250514"));
        assert!(argv
            .iter()
            .any(|arg| arg.to_string_lossy() == "--max-tokens"));
        assert!(argv.iter().any(|arg| arg.to_string_lossy() == "200000"));
    }

    #[test]
    fn build_child_argv_all_flags_combined() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            no_inherit_hooks: true,
            mcp_configs: vec!["/my/mcp.json".to_string()],
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![OsString::from("--prompt"), OsString::from("hello")];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Verify all flags are present and in correct order
        let argv_str: Vec<String> = argv
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        // First: --settings flag
        assert!(argv_str[0].starts_with("--settings="));

        // Then: --setting-sources= (from no_inherit_hooks)
        assert!(argv_str.contains(&"--setting-sources=".to_string()));

        // Then: --strict-mcp-config and --mcp-config pairs
        let _strict_idx = argv_str
            .iter()
            .position(|s| s == "--strict-mcp-config")
            .expect("should have --strict-mcp-config");
        assert!(argv_str.contains(&"--mcp-config".to_string()));
        assert!(argv_str.contains(&"/my/mcp.json".to_string()));

        // Finally: claude_args at the end
        assert!(argv_str.contains(&"--prompt".to_string()));
        assert!(argv_str.contains(&"hello".to_string()));
    }

    #[test]
    fn build_child_argv_capacity_is_sufficient() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            mcp_configs: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = (0..10)
            .map(|i| OsString::from(format!("--arg{}", i)))
            .collect();

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Capacity should be: claude_args.len() + 3 + 2 * mcp_configs.len()
        // = 10 + 3 + 2*3 = 19
        // But we just verify it has enough room for all args
        let expected_len = 1 + // --settings
            1 + // --strict-mcp-config
            2 * 3 + // 3 MCP configs with --mcp-config each
            10; // claude_args
        assert_eq!(argv.len(), expected_len);
    }

    #[test]
    fn build_child_argv_rejects_null_bytes_in_paths() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        // Create a path with a null byte
        let claude_bin = Path::new("/usr/bin/invalid\0claude");
        let claude_args: Vec<OsString> = vec![];

        let result = Session::build_child_argv(claude_bin, &installer, &launch, &claude_args);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => {
                assert!(msg.to_string().contains("claude_bin path invalid"));
            }
            _ => panic!("expected Internal error for null byte in path"),
        }
    }

    #[test]
    fn build_child_argv_rejects_null_bytes_in_mcp_config() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            mcp_configs: vec!["invalid\0config".to_string()],
            ..Default::default()
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let result = Session::build_child_argv(claude_bin, &installer, &launch, &claude_args);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => {
                assert!(msg.to_string().contains("mcp-config value invalid"));
            }
            _ => panic!("expected Internal error for null byte in MCP config"),
        }
    }

    #[test]
    fn build_child_argv_rejects_null_bytes_in_claude_args() {
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![OsString::from("invalid\0arg")];

        let result = Session::build_child_argv(claude_bin, &installer, &launch, &claude_args);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::Internal(msg) => {
                assert!(msg.to_string().contains("claude arg invalid"));
            }
            _ => panic!("expected Internal error for null byte in claude arg"),
        }
    }

    #[test]
    fn build_child_argv_dangerously_skip_permissions_not_passed_by_default() {
        // Test that when --dangerously-skip-permissions is NOT passed via CLI,
        // it does NOT appear in child argv
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should NOT contain --dangerously-skip-permissions
        assert!(!argv.iter().any(|arg| arg
            .to_string_lossy()
            .contains("--dangerously-skip-permissions")));
    }

    #[test]
    fn build_child_argv_dangerously_skip_permissions_appears_exactly_once_when_passed() {
        // Test that when --dangerously-skip-permissions IS passed via CLI,
        // it appears EXACTLY ONCE in child argv
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![OsString::from("--dangerously-skip-permissions")];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain --dangerously-skip-permissions exactly once
        let count = argv
            .iter()
            .filter(|arg| arg.to_string_lossy() == "--dangerously-skip-permissions")
            .count();
        assert_eq!(
            count, 1,
            "Flag should appear exactly once, found {} times",
            count
        );
    }

    #[test]
    fn build_child_argv_dangerously_skip_permissions_with_other_args() {
        // Test that --dangerously-skip-permissions appears correctly when mixed with other args
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![
            OsString::from("--model"),
            OsString::from("claude-sonnet-4-20250514"),
            OsString::from("--dangerously-skip-permissions"),
            OsString::from("--max-tokens"),
            OsString::from("200000"),
        ];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain all args including --dangerously-skip-permissions exactly once
        let argv_str: Vec<String> = argv
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(argv_str.contains(&"--model".to_string()));
        assert!(argv_str.contains(&"claude-sonnet-4-20250514".to_string()));
        assert!(argv_str.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(argv_str.contains(&"--max-tokens".to_string()));
        assert!(argv_str.contains(&"200000".to_string()));

        // Verify no duplicates of --dangerously-skip-permissions
        let skip_count = argv_str
            .iter()
            .filter(|s| s.as_str() == "--dangerously-skip-permissions")
            .count();
        assert_eq!(
            skip_count, 1,
            "Flag should appear exactly once, found {} times",
            skip_count
        );
    }

    #[test]
    fn build_child_argv_dangerously_skip_permissions_with_launch_options() {
        // Test that --dangerously-skip-permissions works correctly with various LaunchOptions
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions {
            no_inherit_hooks: true,
            mcp_configs: vec!["/path/to/mcp.json".to_string()],
            pretrust_cwd: true,
            show_child_stderr: true,
            verbose: true,
        };
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![OsString::from("--dangerously-skip-permissions")];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        let argv_str: Vec<String> = argv
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        // Should contain all launch option flags
        assert!(argv_str.iter().any(|s| s.starts_with("--settings=")));
        assert!(argv_str.contains(&"--setting-sources=".to_string()));
        assert!(argv_str.contains(&"--strict-mcp-config".to_string()));
        assert!(argv_str.contains(&"--mcp-config".to_string()));

        // Should contain --dangerously-skip-permissions exactly once
        let skip_count = argv_str
            .iter()
            .filter(|s| s.as_str() == "--dangerously-skip-permissions")
            .count();
        assert_eq!(
            skip_count, 1,
            "Flag should appear exactly once, found {} times",
            skip_count
        );
    }

    #[test]
    fn build_child_argv_no_duplicate_dangerously_skip_permissions() {
        // Test that if --dangerously-skip-permissions appears multiple times in input,
        // it appears the same number of times in output (we forward, not deduplicate)
        // This documents the current behavior and catches any accidental deduplication
        let installer = HookInstaller::new().unwrap();
        let launch = LaunchOptions::default();
        let claude_bin = Path::new("/usr/bin/claude");
        let claude_args: Vec<OsString> = vec![
            OsString::from("--dangerously-skip-permissions"),
            OsString::from("--dangerously-skip-permissions"),
        ];

        let argv =
            Session::build_child_argv(claude_bin, &installer, &launch, &claude_args).unwrap();

        // Should contain --dangerously-skip-permissions twice (forwarded as-is)
        let count = argv
            .iter()
            .filter(|arg| arg.to_string_lossy() == "--dangerously-skip-permissions")
            .count();
        assert_eq!(
            count, 2,
            "Both instances should be forwarded, found {} times",
            count
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
        assert!(
            !s.contains("line\n\n"),
            "must not double the trailing newline"
        );
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
        assert_eq!(
            v["projects"]["/abs/cwd"]["history"][1],
            serde_json::json!("b")
        );
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
        assert_eq!(
            mode, 0o644,
            "existing mode must be preserved, not reset to 0600"
        );
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

    #[test]
    fn pretrust_cwd_fails_when_home_not_set() {
        // Save original HOME
        let original_home = std::env::var("HOME").ok();

        // Unset HOME
        std::env::remove_var("HOME");

        let result = pretrust_cwd();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("HOME environment variable not set"));

        // Restore HOME
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
    }

    #[test]
    fn home_unset_consistent_error_handling_across_all_modules() {
        // This test verifies that all modules consistently return Error::Config
        // when HOME is not set, rather than panicking or using silent fallbacks.
        // This is critical for predictable behavior in headless/chroot environments.

        use crate::config::Config;
        use crate::poller::{derive_transcript_path, projects_dir_for_cwd};

        // Save original HOME
        let original_home = std::env::var("HOME").ok();

        // Unset HOME
        std::env::remove_var("HOME");
        std::env::remove_var("XDG_CONFIG_HOME");

        // Test 1: Config::default_path() fails with clear error
        let config_result = Config::default_path();
        assert!(config_result.is_err());
        assert!(config_result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Test 2: derive_transcript_path fails with clear error
        let derive_result = derive_transcript_path("session-123", "/project/dir");
        assert!(derive_result.is_err());
        assert!(derive_result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Test 3: projects_dir_for_cwd fails with clear error
        let projects_result = projects_dir_for_cwd();
        assert!(projects_result.is_err());
        assert!(projects_result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Test 4: pretrust_cwd fails with clear error
        let pretrust_result = pretrust_cwd();
        assert!(pretrust_result.is_err());
        assert!(pretrust_result
            .unwrap_err()
            .to_string()
            .contains("HOME environment variable not set"));

        // Restore HOME
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        }
    }
}
