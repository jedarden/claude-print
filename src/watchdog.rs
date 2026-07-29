//! Watchdog timeout mechanism for claude-print.
//!
//! This module implements a comprehensive watchdog that monitors:
//! - Stream-json output from the transcript file
//! - PTY output for first-output detection
//! - Overall session duration (max-turn timeout, applies throughout entire session)
//! - Stop hook execution
//!
//! The watchdog ensures that hung child processes are terminated with
//! proper cleanup (SIGTERM → SIGKILL) and clear diagnostics. The overall
//! timeout prevents indefinite polling of stop.fifo by killing the child
//! and exiting non-zero regardless of why the child wedged.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Default timeout for first stream-json output in seconds.
/// If the child produces no stream-json events within this time, we assume it's hung.
pub const DEFAULT_STREAM_JSON_TIMEOUT_SECS: u64 = 90;

/// Default timeout for PTY first-output in seconds.
/// If the child produces no PTY output within this time, we assume it's hung.
pub const DEFAULT_PTY_TIMEOUT_SECS: u64 = 90;

/// Default overall timeout in seconds (0 = no limit).
pub const DEFAULT_OVERALL_TIMEOUT_SECS: u64 = 3600;

/// Default Stop hook watchdog timeout in seconds.
/// If the Stop hook doesn't fire within this time after prompt injection, the child may be hung.
pub const DEFAULT_STOP_HOOK_TIMEOUT_SECS: u64 = 120;

/// Timeout classification for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutType {
    /// No PTY output received within deadline.
    PtyFirstOutput,
    /// No stream-json output received within deadline.
    StreamJsonFirstOutput,
    /// Overall session timeout exceeded.
    OverallTimeout,
    /// Stop hook didn't fire within deadline after prompt injection.
    StopHookTimeout,
}

impl TimeoutType {
    /// Returns a human-readable description of this timeout type.
    pub fn description(&self) -> &'static str {
        match self {
            Self::PtyFirstOutput => "child produced no PTY output within deadline (process may be hung at startup)",
            Self::StreamJsonFirstOutput => "child produced no stream-json output within deadline (process may be hung during session initialization)",
            Self::OverallTimeout => "session exceeded overall max-turn deadline (max-turn timeout applies throughout entire session)",
            Self::StopHookTimeout => "Stop hook did not fire within deadline after prompt injection (child may have hung during tool use or model inference)",
        }
    }

    /// Returns the error subtype for JSON/stream-json output.
    pub fn subtype(&self) -> &'static str {
        match self {
            Self::PtyFirstOutput => "pty_first_output_timeout",
            Self::StreamJsonFirstOutput => "stream_json_first_output_timeout",
            Self::OverallTimeout => "overall_timeout",
            Self::StopHookTimeout => "stop_hook_timeout",
        }
    }
}

/// Watchdog configuration.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// Timeout for first PTY output in seconds (0 = disabled).
    pub pty_first_output_timeout_secs: u64,
    /// Timeout for first stream-json output in seconds (0 = disabled).
    ///
    /// Only consulted when [`stream_json_mode`](Self::stream_json_mode) is true.
    pub stream_json_first_output_timeout_secs: u64,
    /// Overall session timeout in seconds (0 = disabled).
    pub overall_timeout_secs: u64,
    /// Stop hook watchdog timeout in seconds (0 = disabled).
    pub stop_hook_timeout_secs: u64,
    /// Whether the child is expected to emit stream-json events.
    ///
    /// The stream-json first-output timeout (Phase 2) and its transcript monitor
    /// only apply in stream-json mode. In text/json mode the child never produces
    /// a `<temp_dir>/transcript.jsonl` (the real transcript lands in
    /// `~/.claude/projects/`), and [`WatchdogState::mark_stream_json_output`] is
    /// never called from production, so arming Phase-2 there would be unsatisfiable
    /// and SIGTERM any turn that exceeds the deadline (bf-lu1h). Defaults to
    /// `false` (safe: Phase-2 disabled unless the caller opts in).
    pub stream_json_mode: bool,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            pty_first_output_timeout_secs: DEFAULT_PTY_TIMEOUT_SECS,
            stream_json_first_output_timeout_secs: DEFAULT_STREAM_JSON_TIMEOUT_SECS,
            overall_timeout_secs: DEFAULT_OVERALL_TIMEOUT_SECS,
            stop_hook_timeout_secs: DEFAULT_STOP_HOOK_TIMEOUT_SECS,
            stream_json_mode: false,
        }
    }
}

impl WatchdogConfig {
    /// Create a new config with custom timeouts.
    ///
    /// `stream_json_mode` gates the stream-json first-output timeout (Phase 2) and
    /// its monitor: pass `true` only when `output_format == stream-json`. In any
    /// other mode Phase-2 can never be satisfied, so arming it would spuriously
    /// kill long-running turns (bf-lu1h).
    pub fn new(
        pty_timeout: Option<u64>,
        stream_json_timeout: Option<u64>,
        overall_timeout: Option<u64>,
        stop_hook_timeout: Option<u64>,
        stream_json_mode: bool,
    ) -> Self {
        Self {
            pty_first_output_timeout_secs: pty_timeout.unwrap_or(DEFAULT_PTY_TIMEOUT_SECS),
            stream_json_first_output_timeout_secs: stream_json_timeout
                .unwrap_or(DEFAULT_STREAM_JSON_TIMEOUT_SECS),
            overall_timeout_secs: overall_timeout.unwrap_or(0),
            stop_hook_timeout_secs: stop_hook_timeout.unwrap_or(DEFAULT_STOP_HOOK_TIMEOUT_SECS),
            stream_json_mode,
        }
    }

    /// Returns true if any timeout is configured.
    pub fn has_any_timeout(&self) -> bool {
        self.pty_first_output_timeout_secs > 0
            || self.stream_json_first_output_timeout_secs > 0
            || self.overall_timeout_secs > 0
            || self.stop_hook_timeout_secs > 0
    }
}

/// Watchdog state shared between the main thread and timeout thread.
#[derive(Debug, Clone)]
pub struct WatchdogState {
    /// Whether a timeout has fired.
    timeout_fired: Arc<AtomicBool>,
    /// Type of timeout that fired (0 = none, 1-4 = TimeoutType enum).
    timeout_type: Arc<AtomicU64>,
    /// Whether PTY output has been received.
    pty_output_received: Arc<AtomicBool>,
    /// Whether stream-json output has been received.
    stream_json_output_received: Arc<AtomicBool>,
    /// When the prompt was injected (None = not injected yet).
    prompt_injected_at: Arc<std::sync::Mutex<Option<Instant>>>,
    /// Session start time.
    session_start: Arc<AtomicBool>,
}

impl WatchdogState {
    /// Create a new watchdog state.
    pub fn new() -> Self {
        Self {
            timeout_fired: Arc::new(AtomicBool::new(false)),
            timeout_type: Arc::new(AtomicU64::new(0)),
            pty_output_received: Arc::new(AtomicBool::new(false)),
            stream_json_output_received: Arc::new(AtomicBool::new(false)),
            prompt_injected_at: Arc::new(std::sync::Mutex::new(None)),
            session_start: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark that PTY output has been received.
    pub fn mark_pty_output(&self) {
        self.pty_output_received.store(true, Ordering::SeqCst);
    }

    /// Mark that stream-json output has been received.
    pub fn mark_stream_json_output(&self) {
        self.stream_json_output_received
            .store(true, Ordering::SeqCst);
    }

    /// Mark that the prompt has been injected.
    pub fn mark_prompt_injected(&self) {
        *self.prompt_injected_at.lock().unwrap() = Some(Instant::now());
    }

    /// Mark that the session has started.
    pub fn mark_session_start(&self) {
        self.session_start.store(true, Ordering::SeqCst);
    }

    /// Check if a timeout has fired.
    pub fn has_timeout_fired(&self) -> bool {
        self.timeout_fired.load(Ordering::SeqCst)
    }

    /// Get the timeout type that fired.
    pub fn get_timeout_type(&self) -> Option<TimeoutType> {
        match self.timeout_type.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(TimeoutType::PtyFirstOutput),
            2 => Some(TimeoutType::StreamJsonFirstOutput),
            3 => Some(TimeoutType::OverallTimeout),
            4 => Some(TimeoutType::StopHookTimeout),
            _ => None,
        }
    }

    /// Internal: fire a timeout.
    ///
    /// Test-only — production code sets the underlying atomics directly from the
    /// watchdog thread (see the timeout handler). This helper exists so tests can
    /// simulate a fired timeout without spawning a thread.
    #[cfg(test)]
    fn fire_timeout(&self, timeout_type: TimeoutType) {
        self.timeout_fired.store(true, Ordering::SeqCst);
        let type_code = match timeout_type {
            TimeoutType::PtyFirstOutput => 1,
            TimeoutType::StreamJsonFirstOutput => 2,
            TimeoutType::OverallTimeout => 3,
            TimeoutType::StopHookTimeout => 4,
        };
        self.timeout_type.store(type_code, Ordering::SeqCst);
    }
}

impl Default for WatchdogState {
    fn default() -> Self {
        Self::new()
    }
}

/// Watchdog instance that monitors the child process.
#[derive(Debug)]
pub struct Watchdog {
    /// Watchdog configuration.
    config: WatchdogConfig,
    /// Shared state.
    state: WatchdogState,
    /// Child process PID.
    child_pid: nix::unistd::Pid,
    /// Temp directory path where transcript will be written.
    temp_dir_path: Option<PathBuf>,
    /// Self-pipe write end raw fd for signaling the event loop on timeout.
    self_pipe_write_fd: Option<i32>,
}

impl Watchdog {
    /// Create a new watchdog.
    pub fn new(
        config: WatchdogConfig,
        child_pid: nix::unistd::Pid,
        temp_dir_path: Option<PathBuf>,
        self_pipe_write_fd: Option<i32>,
    ) -> Self {
        Self {
            config,
            state: WatchdogState::new(),
            child_pid,
            temp_dir_path,
            self_pipe_write_fd,
        }
    }

    /// Get the shared state for use in the main thread.
    pub fn state(&self) -> &WatchdogState {
        &self.state
    }

    /// Spawn the watchdog timeout thread.
    ///
    /// The thread monitors:
    /// 1. PTY first-output timeout
    /// 2. Stream-json first-output timeout (if temp_dir_path provided)
    /// 3. Overall session timeout
    /// 4. Stop hook watchdog timeout (after prompt injection)
    ///
    /// Returns a thread handle that should be dropped (not joined) - the thread
    /// runs until timeout or completion, and late SIGTERMs to a dead child are harmless.
    pub fn spawn_timeout_thread(&self) -> thread::JoinHandle<()> {
        let config = self.config.clone();
        let child_pid = self.child_pid;
        let timeout_fired = Arc::clone(&self.state.timeout_fired);
        let timeout_type = Arc::clone(&self.state.timeout_type);
        let pty_output_received = Arc::clone(&self.state.pty_output_received);
        let stream_json_output_received = Arc::clone(&self.state.stream_json_output_received);
        let prompt_injected_at = Arc::clone(&self.state.prompt_injected_at);
        let session_start = Arc::clone(&self.state.session_start);
        let temp_dir_path = self.temp_dir_path.clone();
        // Copy the raw fd for signaling the event loop
        let self_pipe_write_fd = self.self_pipe_write_fd;

        thread::spawn(move || {
            let session_start_time = Instant::now();
            session_start.store(true, Ordering::SeqCst);

            // Spawn stream-json monitor ONLY in stream-json mode (bf-lu1h). In
            // text/json mode there is no <temp_dir>/transcript.jsonl to watch —
            // the real transcript lives in ~/.claude/projects/ — so the monitor
            // would poll forever for a file that never appears, and the Phase-2
            // deadline it feeds would spuriously SIGTERM the child.
            let _stream_json_monitor = if config.stream_json_mode {
                temp_dir_path.as_ref().map(|dir| {
                    spawn_stream_json_monitor_in_dir(
                        dir.clone(),
                        Arc::clone(&stream_json_output_received),
                    )
                })
            } else {
                None
            };

            loop {
                // Check if already fired
                if timeout_fired.load(Ordering::SeqCst) {
                    return;
                }

                let elapsed = session_start_time.elapsed();

                // Get current state
                let has_pty_output = pty_output_received.load(Ordering::SeqCst);
                let has_stream_json_output = stream_json_output_received.load(Ordering::SeqCst);
                let prompt_injected = { *prompt_injected_at.lock().unwrap() };

                // Check Phase 1: PTY first-output timeout
                if config.pty_first_output_timeout_secs > 0
                    && !has_pty_output
                    && elapsed >= Duration::from_secs(config.pty_first_output_timeout_secs)
                {
                    let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(1, Ordering::SeqCst); // PtyFirstOutput
                                                             // Signal the event loop via self-pipe
                    if let Some(fd) = self_pipe_write_fd {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
                        }
                    }
                    return;
                }

                // Check Phase 2: Stream-json first-output timeout.
                // Gated on stream-json mode (bf-lu1h): outside stream-json the
                // child never produces <temp_dir>/transcript.jsonl and
                // mark_stream_json_output is never called, so this deadline is
                // unsatisfiable and must not fire.
                if config.stream_json_mode
                    && config.stream_json_first_output_timeout_secs > 0
                    && !has_stream_json_output
                    && elapsed >= Duration::from_secs(config.stream_json_first_output_timeout_secs)
                {
                    let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(2, Ordering::SeqCst); // StreamJsonFirstOutput
                                                             // Signal the event loop via self-pipe
                    if let Some(fd) = self_pipe_write_fd {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
                        }
                    }
                    return;
                }

                // Check Phase 3: Overall timeout (applies throughout entire session)
                if config.overall_timeout_secs > 0
                    && elapsed >= Duration::from_secs(config.overall_timeout_secs)
                {
                    let _ = nix::sys::signal::kill(child_pid, nix::sys::signal::Signal::SIGTERM);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(3, Ordering::SeqCst); // OverallTimeout
                                                             // Signal the event loop via self-pipe
                    if let Some(fd) = self_pipe_write_fd {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
                        }
                    }
                    return;
                }

                // Check Phase 4: Stop hook watchdog timeout (after prompt injected)
                if config.stop_hook_timeout_secs > 0 {
                    if let Some(injected_time) = prompt_injected {
                        let time_since_injection = injected_time.elapsed();
                        if time_since_injection
                            >= Duration::from_secs(config.stop_hook_timeout_secs)
                        {
                            let _ = nix::sys::signal::kill(
                                child_pid,
                                nix::sys::signal::Signal::SIGTERM,
                            );
                            timeout_fired.store(true, Ordering::SeqCst);
                            timeout_type.store(4, Ordering::SeqCst); // StopHookTimeout
                                                                     // Signal the event loop via self-pipe
                            if let Some(fd) = self_pipe_write_fd {
                                let byte: [u8; 1] = [1];
                                unsafe {
                                    let _ =
                                        libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
                                }
                            }
                            return;
                        }
                    }
                }

                // Sleep a bit before next check
                thread::sleep(Duration::from_millis(100));
            }
        })
    }

    /// Fire a timeout manually (for testing).
    #[cfg(test)]
    pub fn fire_timeout(&self, timeout_type: TimeoutType) {
        self.state.fire_timeout(timeout_type);
    }
}

/// Spawn a background thread that monitors the temp directory for stream-json events.
///
/// This thread wakes up every 100ms to check if the transcript file exists in the
/// temp directory and contains any valid JSON lines. When it finds stream-json output,
/// it sets the flag and exits.
///
/// The transcript file is expected to be at <temp_dir>/transcript.jsonl
fn spawn_stream_json_monitor_in_dir(
    temp_dir: PathBuf,
    output_received: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Check if file exists and has content
        let mut last_size = 0u64;
        let transcript_path = temp_dir.join("transcript.jsonl");

        loop {
            // Exit if already received output
            if output_received.load(Ordering::SeqCst) {
                return;
            }

            // Try to read the transcript file
            if let Ok(metadata) = std::fs::metadata(&transcript_path) {
                let current_size = metadata.len();

                // If file has grown, check for content
                if current_size > last_size {
                    if let Ok(file) = std::fs::File::open(&transcript_path) {
                        use std::io::{BufRead, BufReader};
                        let reader = BufReader::new(file);

                        // Check each line for valid JSON
                        for line in reader.lines().map_while(Result::ok) {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                // Try to parse as JSON
                                if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
                                    output_received.store(true, Ordering::SeqCst);
                                    return;
                                }
                            }
                        }
                    }

                    last_size = current_size;
                }
            }

            // Sleep before next check
            thread::sleep(Duration::from_millis(100));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_type_descriptions() {
        assert!(TimeoutType::PtyFirstOutput.description().contains("PTY"));
        assert!(TimeoutType::StreamJsonFirstOutput
            .description()
            .contains("stream-json"));
        assert!(TimeoutType::OverallTimeout
            .description()
            .contains("overall"));
        assert!(TimeoutType::StopHookTimeout
            .description()
            .contains("Stop hook"));
    }

    #[test]
    fn test_timeout_type_subtypes() {
        assert_eq!(
            TimeoutType::PtyFirstOutput.subtype(),
            "pty_first_output_timeout"
        );
        assert_eq!(
            TimeoutType::StreamJsonFirstOutput.subtype(),
            "stream_json_first_output_timeout"
        );
        assert_eq!(TimeoutType::OverallTimeout.subtype(), "overall_timeout");
        assert_eq!(TimeoutType::StopHookTimeout.subtype(), "stop_hook_timeout");
    }

    #[test]
    fn test_watchdog_config_default() {
        let config = WatchdogConfig::default();
        assert_eq!(
            config.pty_first_output_timeout_secs,
            DEFAULT_PTY_TIMEOUT_SECS
        );
        assert_eq!(
            config.stream_json_first_output_timeout_secs,
            DEFAULT_STREAM_JSON_TIMEOUT_SECS
        );
        assert_eq!(config.overall_timeout_secs, DEFAULT_OVERALL_TIMEOUT_SECS);
        assert_eq!(
            config.stop_hook_timeout_secs,
            DEFAULT_STOP_HOOK_TIMEOUT_SECS
        );
        // Default must be safe: stream-json Phase-2 disabled unless opted in.
        assert!(!config.stream_json_mode);
    }

    #[test]
    fn test_watchdog_config_custom() {
        let config = WatchdogConfig::new(Some(30), Some(60), Some(120), Some(90), true);
        assert_eq!(config.pty_first_output_timeout_secs, 30);
        assert_eq!(config.stream_json_first_output_timeout_secs, 60);
        assert_eq!(config.overall_timeout_secs, 120);
        assert_eq!(config.stop_hook_timeout_secs, 90);
        assert!(config.stream_json_mode);
    }

    #[test]
    fn test_watchdog_state() {
        let state = WatchdogState::new();
        assert!(!state.has_timeout_fired());
        assert!(state.get_timeout_type().is_none());

        state.mark_pty_output();
        assert!(!state.has_timeout_fired()); // Should not fire automatically

        state.mark_prompt_injected();
        assert!(!state.has_timeout_fired());
    }

    #[test]
    fn test_watchdog_state_fire_timeout() {
        let state = WatchdogState::new();
        assert!(!state.has_timeout_fired());

        state.fire_timeout(TimeoutType::StreamJsonFirstOutput);
        assert!(state.has_timeout_fired());
        assert_eq!(
            state.get_timeout_type(),
            Some(TimeoutType::StreamJsonFirstOutput)
        );
    }

    // ── bf-lu1h: Phase-2 stream-json gating ───────────────────────────────────
    //
    // The stream-json first-output timeout is unsatisfiable outside stream-json
    // mode: the child never writes <temp_dir>/transcript.jsonl (the real
    // transcript lives in ~/.claude/projects/), and mark_stream_json_output is
    // never called from production. Arming Phase-2 for every output format
    // therefore SIGTERMs any turn longer than the deadline. These tests pin the
    // fix: Phase-2 must only fire in stream-json mode.

    /// Spawn a long-lived child the watchdog would SIGTERM if Phase-2 fired.
    /// Returns the Rust `Child` handle (which owns the pid for cleanup) and the
    /// `nix::Pid` the watchdog signals.
    fn spawn_sleep_child() -> (std::process::Child, nix::unistd::Pid) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("`sleep` should be spawnable on PATH");
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        (child, pid)
    }

    /// bf-lu1h negative: a configured stream-json timeout must NOT fire outside
    /// stream-json mode, even after the deadline elapses with no transcript
    /// present. PTY output is received so Phase-1 is satisfied; only Phase-2
    /// could fire, and the gate must prevent it — leaving the child alive.
    #[test]
    fn stream_json_timeout_does_not_fire_outside_stream_json_mode() {
        // Stream-json timeout is set (1s), but mode=false (text/json). A real
        // transcript.jsonl is absent (no temp dir provided → no monitor either).
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), false);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None, None);
        let state = watchdog.state();
        let _handle = watchdog.spawn_timeout_thread();

        // Satisfy Phase-1 so only the gated Phase-2 could fire.
        state.mark_pty_output();

        // Wait well past the 1s stream-json deadline (100ms poll granularity).
        std::thread::sleep(Duration::from_millis(2000));

        assert_ne!(
            state.get_timeout_type(),
            Some(TimeoutType::StreamJsonFirstOutput),
            "stream-json timeout fired outside stream-json mode"
        );
        assert!(
            !state.has_timeout_fired(),
            "no timeout should fire in text mode once PTY output arrived"
        );

        // The child must still be alive (not killed by a spurious Phase-2 SIGTERM).
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!("child should still be alive, but exited with {status}"),
            Err(e) => panic!("try_wait failed: {e}"),
        }

        // Cleanup.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// bf-lu1h positive: the same configured stream-json timeout DOES fire in
    /// stream-json mode when no transcript appears, confirming the gate enables
    /// Phase-2 (regression guard against the gate becoming always-false).
    #[test]
    fn stream_json_timeout_fires_in_stream_json_mode() {
        let dir = tempfile::TempDir::new().unwrap();
        // temp dir has NO transcript.jsonl → monitor never sets the flag.
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), true);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, Some(dir.path().to_path_buf()), None);
        let state = watchdog.state();
        let _handle = watchdog.spawn_timeout_thread();

        // Satisfy Phase-1 so Phase-2 is the one that fires.
        state.mark_pty_output();

        std::thread::sleep(Duration::from_millis(2000));

        assert!(
            state.has_timeout_fired(),
            "stream-json timeout should fire in stream-json mode with no transcript"
        );
        assert_eq!(
            state.get_timeout_type(),
            Some(TimeoutType::StreamJsonFirstOutput),
        );

        // The watchdog SIGTERM'd the child; reap it.
        let _ = child.kill();
        let _ = child.wait();
    }
}
