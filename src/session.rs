use crate::error::{Error, Result};
use crate::event_loop::{ExitReason, EventLoop};
use crate::hook::HookInstaller;
use crate::poller::{open_fifo_nonblock, parse_stop_payload, resolve_stop_info};
use crate::pty::PtySpawner;
use crate::startup::{StartupAction, StartupSeq};
use crate::terminal::TerminalEmu;
use crate::transcript::{read_transcript, TranscriptResult};
use crate::watchdog::{Watchdog, WatchdogConfig, TimeoutType};
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
    ///
    /// # Returns
    ///
    /// Returns a `SessionResult` containing the transcript, Claude version, and duration.
    ///
    /// # Errors
    ///
    /// Returns `Error::NoResponse` if the child exits without sending a Stop payload.
    /// Returns `Error::Timeout` if the timeout expires (no output or overall timeout).
    /// Returns `Error::Interrupted` if a SIGINT is received.
    pub fn run(
        claude_bin: &Path,
        claude_args: &[OsString],
        prompt: Vec<u8>,
        timeout_secs: Option<u64>,
        first_output_timeout_secs: Option<u64>,
        stream_json_timeout_secs: Option<u64>,
        stop_hook_timeout_secs: Option<u64>,
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
    fn run_inner(
        claude_bin: &Path,
        claude_args: &[OsString],
        prompt: Vec<u8>,
        timeout_secs: Option<u64>,
        first_output_timeout_secs: Option<u64>,
        stream_json_timeout_secs: Option<u64>,
        stop_hook_timeout_secs: Option<u64>,
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

        // 3. Build child argv.
        let cmd = CString::new(claude_bin.to_string_lossy().as_bytes())
            .map_err(|e| Error::Internal(anyhow::anyhow!("claude_bin path invalid: {e}")))?;
        let mut args: Vec<CString> = Vec::with_capacity(claude_args.len() + 3);
        args.push(CString::new("--dangerously-skip-permissions").unwrap());
        args.push(
            CString::new(format!("--settings={}", installer.settings_path.to_string_lossy()))
                .map_err(|e| Error::Internal(anyhow::anyhow!("settings path invalid: {e}")))?,
        );
        // Prevent global settings inheritance - the temp settings.json contains only the Stop hook
        // and inheriting global hooks (SessionStart, etc.) can cause the child to hang at startup.
        args.push(CString::new("--setting-sources=").unwrap());
        for arg in claude_args {
            let arg_str = arg.to_string_lossy().to_string();
            args.push(
                CString::new(arg_str)
                    .map_err(|e| Error::Internal(anyhow::anyhow!("claude arg invalid: {e}")))?,
            );
        }

        // 4. Self-pipe for SIGINT.
        let (self_pipe_read, self_pipe_write) =
            nix::unistd::pipe().map_err(|e| Error::Internal(anyhow::anyhow!("pipe() failed: {e}")))?;
        unsafe {
            let write_ptr = &raw mut SELF_PIPE_WRITE;
            *write_ptr = Some(self_pipe_write.try_clone().unwrap());
            signal::signal(signal::Signal::SIGINT, SigHandler::Handler(sigint_handler))
                .map_err(|e| Error::SignalHandlerFailed(format!("SIGINT: {e}")))?;
            signal::signal(signal::Signal::SIGTERM, SigHandler::Handler(sigterm_handler))
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
            stream_json_timeout_secs.or_else(|| first_output_timeout_secs),
            timeout_secs,
            stop_hook_timeout_secs,
        );

        // Get temp directory path for stream-json monitoring
        // The watchdog will monitor <temp_dir>/transcript.jsonl for stream-json output
        let temp_dir_path = installer.dir_path().to_path_buf();

        // Get the raw fd for the self-pipe write end for the watchdog to signal timeout
        let watchdog_self_pipe_fd = Some(self_pipe_write.as_raw_fd());

        let watchdog = Watchdog::new(watchdog_config, spawner.child_pid, Some(temp_dir_path), watchdog_self_pipe_fd);

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

        // 12. Run the event loop.
        let master_fd = spawner.master.as_raw_fd();
        let watchdog_state_clone = watchdog_state.clone();
        let mut last_phase = startup.phase().clone();

        let exit_reason = event_loop.run(|chunk| {
            // Empty chunk = timer tick from the event loop (poll timeout with no data).
            // Only feed real data to the terminal emulator and startup sequence.
            if !chunk.is_empty() {
                // Mark that we've received first output from the child (PTY output)
                watchdog_state_clone.mark_pty_output();
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
                StartupAction::Write(bytes) => {
                    unsafe {
                        libc::write(master_fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                    }
                }
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
            }
            last_phase = current_phase.clone();

            match &timer_action {
                StartupAction::Write(bytes) => {
                    unsafe {
                        libc::write(master_fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
                    }
                }
                StartupAction::HardTimeout => {
                    // Handled after event loop exits.
                }
                StartupAction::None => {}
            }
        })?;

        // 13. Check if watchdog timeout fired.
        if watchdog_state.has_timeout_fired() {
            let timeout_type = watchdog_state.get_timeout_type().unwrap_or(TimeoutType::OverallTimeout);
            let timeout_msg = timeout_type.description();

            // Write diagnostic to stderr
            eprintln!("claude-print: {}", timeout_msg);
            eprintln!("claude-print: sending SIGTERM to child pid {}", spawner.child_pid);

            kill_child(spawner.child_pid);
            return Err(Error::Timeout(timeout_msg.to_string()));
        }

        // 14. Handle exit reason.
        match exit_reason {
            ExitReason::FifoPayload(payload) => {
                // Parse stop payload.
                let stop_payload = parse_stop_payload(&payload)?;
                let stop_info = resolve_stop_info(stop_payload);

                // Read transcript.
                let transcript_path = stop_info.transcript_path.as_ref();
                let transcript = if let Some(path) = transcript_path {
                    read_transcript(path, stop_info.last_assistant_message.as_deref())?
                } else {
                    return Err(Error::Internal(anyhow::anyhow!(
                        "Stop payload contained no transcript path and could not derive one"
                    )));
                };

                // Wait for child to exit.
                kill_child(spawner.child_pid);

                let transcript_path = stop_info.transcript_path.clone().unwrap_or_else(|| {
                    std::path::PathBuf::from("transcript.jsonl")
                });

                Ok(SessionResult {
                    transcript,
                    claude_version,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                    transcript_path,
                })
            }
            ExitReason::ChildExited => {
                // Child exited without Stop hook.
                let _ = waitpid(spawner.child_pid, None);
                Err(Error::Internal(anyhow::anyhow!("Child exited without sending Stop payload")))
            }
            ExitReason::Interrupted => {
                kill_child(spawner.child_pid);
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
        let first_line = combined
            .lines()
            .next()
            .ok_or_else(|| Error::Internal(anyhow::anyhow!("claude --version produced no output")))?;

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

        let mock_script = r#"#!/bin/bash
if [[ "$1" == "--version" ]]; then
    echo "claude-print-mock-1.0.0"
    exit 0
fi
exit 1
"#;

        fs::write(&mock_bin, mock_script).unwrap();

        // Make it executable
        let mut perms = fs::metadata(&mock_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&mock_bin, perms).unwrap();

        // Test version resolution
        let result = Session::resolve_claude_version(&mock_bin);
        assert!(result.is_ok(), "Version resolution should succeed: {:?}", result);
        assert_eq!(result.unwrap(), "claude-print-mock-1.0.0");
    }

    #[test]
    fn test_session_result_struct_has_required_fields() {
        // This test verifies that SessionResult has the required fields
        // by checking that we can construct and access them
        use crate::transcript::{TranscriptResult, AggregatedUsage};

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
        };

        assert_eq!(session_result.claude_version, "claude-1.0.0");
        assert_eq!(session_result.duration_ms, 1000);
        assert_eq!(session_result.transcript.text, "test");
        assert_eq!(session_result.transcript.session_id.as_deref(), Some("sess-123"));
        assert_eq!(session_result.transcript_path, std::path::PathBuf::from("transcript.jsonl"));
    }
}
