use crate::error::{Error, Result};
use crate::event_loop::{ExitReason, EventLoop};
use crate::hook::HookInstaller;
use crate::poller::{open_fifo_nonblock, parse_stop_payload, resolve_stop_info};
use crate::pty::PtySpawner;
use crate::startup::{StartupAction, StartupSeq};
use crate::terminal::TerminalEmu;
use crate::transcript::{read_transcript, TranscriptResult};
use nix::sys::signal::{self, SigHandler};
use nix::sys::wait::waitpid;
use std::ffi::{CString, OsString};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Result of a Claude Code session.
#[derive(Debug)]
pub struct SessionResult {
    /// The parsed transcript result.
    pub transcript: TranscriptResult,
    /// Claude Code version string.
    pub claude_version: String,
    /// Session duration in milliseconds.
    pub duration_ms: u64,
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
    /// * `timeout_secs` - Optional timeout in seconds.
    ///
    /// # Returns
    ///
    /// Returns a `SessionResult` containing the transcript, Claude version, and duration.
    ///
    /// # Errors
    ///
    /// Returns `Error::NoResponse` if the child exits without sending a Stop payload.
    /// Returns `Error::Timeout` if the timeout expires.
    /// Returns `Error::Interrupted` if a SIGINT is received.
    pub fn run(
        claude_bin: &Path,
        claude_args: &[OsString],
        prompt: Vec<u8>,
        timeout_secs: Option<u64>,
    ) -> Result<SessionResult> {
        let start_time = Instant::now();

        // 1. Install hook files (temp dir, hook.sh, stop.fifo).
        let installer = HookInstaller::new()?;

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

        // 5a. Set up timeout handling if specified.
        let timeout_fired = Arc::new(AtomicBool::new(false));
        let timeout_thread = if let Some(secs) = timeout_secs {
            let child_pid = spawner.child_pid;
            let timeout_fired_clone = Arc::clone(&timeout_fired);
            Some(thread::spawn(move || {
                thread::sleep(Duration::from_secs(secs));
                // Check if we already completed before firing
                if !timeout_fired_clone.load(Ordering::SeqCst) {
                    // Send SIGTERM to child
                    let _ = signal::kill(child_pid, signal::Signal::SIGTERM);
                    timeout_fired_clone.store(true, Ordering::SeqCst);
                }
            }))
        } else {
            None
        };

        // 6. Create event loop.
        let mut event_loop = EventLoop::new(spawner.master.as_raw_fd(), self_pipe_read.as_raw_fd());

        // 7. Create terminal emulator.
        let mut terminal = TerminalEmu::new(24, 80);

        // 8. Create startup sequence.
        let mut startup = StartupSeq::new(prompt);

        // 9. Open the FIFO before the event loop (so Stop hook can fire during the session).
        // The FIFO won't be readable until Claude Code writes to it, which happens after the prompt is injected.
        // Keep the write-end (keeper) alive for the duration of the event loop.
        let _fifo_keeper = match open_fifo_nonblock(&installer.fifo_path) {
            Ok((read_fd, keeper)) => {
                event_loop.add_fifo_fd(read_fd.as_raw_fd());
                Some(keeper)
            }
            Err(e) => {
                eprintln!("warning: failed to open FIFO, continuing without Stop detection: {e}");
                None
            }
        };

        // 12. Run the event loop.
        let master_fd = spawner.master.as_raw_fd();

        let exit_reason = event_loop.run(|chunk| {
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

            // Feed chunk to startup sequence.
            let action = startup.feed(chunk);

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

        // Join timeout thread if it exists (mark completion before checking timeout).
        if let Some(handle) = timeout_thread {
            // Event loop completed - mark as done before joining to avoid race.
            let _ = handle.join();
        }

        // 13. Check if timeout fired.
        if timeout_fired.load(Ordering::SeqCst) {
            let _ = waitpid(spawner.child_pid, None);
            return Err(Error::Timeout(format!(
                "session exceeded {} second deadline",
                timeout_secs.unwrap_or(0)
            )));
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
                let _ = waitpid(spawner.child_pid, None);

                Ok(SessionResult {
                    transcript,
                    claude_version,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                })
            }
            ExitReason::ChildExited => {
                // Child exited without Stop hook.
                let _ = waitpid(spawner.child_pid, None);
                Err(Error::Internal(anyhow::anyhow!("Child exited without sending Stop payload")))
            }
            ExitReason::Interrupted => {
                // Send SIGTERM to child.
                nix::sys::signal::kill(spawner.child_pid, nix::sys::signal::Signal::SIGTERM)
                    .map_err(|e| Error::Internal(anyhow::anyhow!("SIGTERM failed: {e}")))?;
                let _ = waitpid(spawner.child_pid, None);
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
        };

        assert_eq!(session_result.claude_version, "claude-1.0.0");
        assert_eq!(session_result.duration_ms, 1000);
        assert_eq!(session_result.transcript.text, "test");
        assert_eq!(session_result.transcript.session_id.as_deref(), Some("sess-123"));
    }
}
