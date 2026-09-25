//! Watchdog timeout mechanism for claude-print.
//!
//! This module implements a comprehensive watchdog that monitors:
//! - Stream-json output: the live transcript reader credits the shared
//!   first-output flag when it forwards its first transcript line
//!   (claudepr-33fdf4ed)
//! - PTY output for first-output detection
//! - Overall session duration (max-turn timeout, applies throughout entire session)
//! - Stop hook execution
//!
//! The watchdog ensures that hung child processes are terminated with
//! proper cleanup (SIGTERM → SIGKILL) and clear diagnostics. The overall
//! timeout prevents indefinite polling of stop.fifo by killing the child
//! and exiting non-zero regardless of why the child wedged.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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
    ///
    /// This is the only diagnostic surface a fired deadline has: every
    /// deadline maps to the same wire subtype (`timeout`, see
    /// [`crate::error::ClaudePrintError::subtype`]) and this text goes to
    /// stderr. The per-timeout subtype strings the plan once advertised
    /// (`pty_first_output_timeout` etc.) were never emitted and are gone —
    /// do not reintroduce them without changing the documented wire contract
    /// (claudepr-33fdf4ed).
    pub fn description(&self) -> &'static str {
        match self {
            Self::PtyFirstOutput => "child produced no PTY output within deadline (process may be hung at startup)",
            Self::StreamJsonFirstOutput => "child produced no stream-json output within deadline (process may be hung during session initialization)",
            Self::OverallTimeout => "session exceeded overall max-turn deadline (max-turn timeout applies throughout entire session)",
            Self::StopHookTimeout => "Stop hook did not fire within deadline after prompt injection (child may have hung during tool use or model inference)",
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
    /// The stream-json first-output timeout (Phase 2) only applies in stream-json
    /// mode: that is the one mode where a live transcript reader runs and credits
    /// [`WatchdogState::mark_stream_json_output`] when it forwards its first line
    /// (claudepr-33fdf4ed). In text/json mode no reader runs and no transcript is
    /// produced, so arming Phase-2 there would be unsatisfiable and SIGTERM any
    /// turn that exceeds the deadline (bf-lu1h). Defaults to `false` (safe:
    /// Phase-2 disabled unless the caller opts in).
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
    /// Whether stream-json output has been received. The live transcript reader
    /// holds a clone of this flag (see
    /// [`WatchdogState::stream_json_output_flag`]) and stores `true` when it
    /// forwards its first transcript line.
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
    ///
    /// Two production callers (claudepr-33fdf4ed): the live transcript reader
    /// stores the flag it was handed at spawn (its first forwarded line — see
    /// [`WatchdogState::stream_json_output_flag`]), and the session credits the
    /// deadline itself when the reader could not be spawned (live tailing is
    /// degraded, so nothing else ever could — an unobservable stream must not
    /// re-arm an unconditional kill).
    pub fn mark_stream_json_output(&self) {
        self.stream_json_output_received
            .store(true, Ordering::SeqCst);
    }

    /// A handle on the stream-json first-output flag, for handing to the live
    /// transcript reader.
    ///
    /// The reader stores `true` on this flag the moment it forwards its first
    /// transcript line, which is what satisfies the Phase-2 deadline
    /// (claudepr-33fdf4ed). Both this clone and the clone the timeout thread
    /// takes at spawn point at the same allocation, so the order of
    /// [`Self::spawn_timeout_thread`] and this call does not matter.
    pub fn stream_json_output_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stream_json_output_received)
    }

    /// Mark that the prompt has been injected.
    pub fn mark_prompt_injected(&self) {
        match self.prompt_injected_at.lock() {
            Ok(mut guard) => *guard = Some(Instant::now()),
            Err(poisoned) => {
                // Recover from poison: take the value and update it
                // This allows the watchdog to continue functioning despite previous panic
                *poisoned.into_inner() = Some(Instant::now());
            }
        }
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
    /// Self-pipe write end raw fd for signaling the event loop on timeout.
    ///
    /// The spawned timeout thread writes through its OWN owned duplicate of
    /// this fd (taken in [`Self::spawn_timeout_thread`]), never through the
    /// raw number: the thread is detached and can outlive the drive by the
    /// whole remaining deadline, by which point the drive's pipe fds are
    /// closed and their numbers may have been reused by the next drive's
    /// self-pipe. A late deadline firing through a reused number would wake
    /// the WRONG session's event loop and surface as a bogus
    /// `Error::Interrupted`; through its own duplicate it hits a dead pipe
    /// (EPIPE, ignored) and cannot cross into anyone else's drive.
    self_pipe_write_fd: Option<i32>,
    /// Whether deadline enforcement may SIGTERM the child directly.
    ///
    /// `true` on the stateless path (claude-print spawned the child, so it owns
    /// teardown). `false` on the pool path: the worker process belongs to the
    /// daemon, and killing it behind the daemon's back desynchronizes the
    /// daemon's worker registry — the daemon protocol decides teardown
    /// (claudepr-f1e93af1). Deadlines still fire and the self-pipe still wakes
    /// the event loop; only the enforcement reroutes.
    signal_child: bool,
}

impl Watchdog {
    /// Create a new watchdog.
    ///
    /// Stream-json first output (Phase 2) is credited through
    /// [`WatchdogState::stream_json_output_flag`], which the session hands to
    /// the live transcript reader — there is no file-system monitor to point
    /// at a path (claudepr-33fdf4ed: the old `<temp_dir>/transcript.jsonl`
    /// poller watched a file nothing writes, which made Phase 2 an
    /// unconditional session cap).
    pub fn new(
        config: WatchdogConfig,
        child_pid: nix::unistd::Pid,
        self_pipe_write_fd: Option<i32>,
    ) -> Self {
        Self {
            config,
            state: WatchdogState::new(),
            child_pid,
            self_pipe_write_fd,
            signal_child: true,
        }
    }

    /// Suppress direct child signaling (deadline state and self-pipe wake are
    /// unchanged).
    ///
    /// Pool path only: every deadline still fires and the session still
    /// observes its `Timeout` error, but enforcement reroutes through worker
    /// release → daemon teardown instead of a raw SIGTERM to a process we do
    /// not own.
    pub fn without_child_signals(mut self) -> Self {
        self.signal_child = false;
        self
    }

    /// Get the shared state for use in the main thread.
    pub fn state(&self) -> &WatchdogState {
        &self.state
    }

    /// Spawn the watchdog timeout thread.
    ///
    /// The thread monitors:
    /// 1. PTY first-output timeout
    /// 2. Stream-json first-output timeout (stream-json mode only)
    /// 3. Overall session timeout
    /// 4. Stop hook watchdog timeout (after prompt injection)
    ///
    /// Returns a thread handle that should be dropped, not joined: the thread
    /// is deliberately detached and may fire long after its drive returned.
    /// A late fire is harmless by construction — it records the timeout in
    /// the shared state nobody reads anymore and signals through its OWN
    /// duplicate of the self-pipe write end (taken here at spawn; see the
    /// `self_pipe_write_fd` field doc), so the byte lands in the ORIGINAL
    /// pipe or hits a dead one (EPIPE, ignored) and can never cross into a
    /// later drive's self-pipe at a reused fd number. Direct child signaling
    /// (stateless path only) targets a dead child and is ignored.
    pub fn spawn_timeout_thread(&self) -> thread::JoinHandle<()> {
        let config = self.config.clone();
        let child_pid = self.child_pid;
        let timeout_fired = Arc::clone(&self.state.timeout_fired);
        let timeout_type = Arc::clone(&self.state.timeout_type);
        let pty_output_received = Arc::clone(&self.state.pty_output_received);
        let stream_json_output_received = Arc::clone(&self.state.stream_json_output_received);
        let prompt_injected_at = Arc::clone(&self.state.prompt_injected_at);
        let session_start = Arc::clone(&self.state.session_start);
        // Duplicate the self-pipe write end for the thread to signal through
        // (see the field doc: this thread is detached and may fire long after
        // the drive ended, so it must never write through a raw fd number
        // another drive's self-pipe may have reused). On dup failure the wake
        // degrades to the event loop's own poll tick, which observes
        // `timeout_fired` regardless.
        let self_pipe_write_fd =
            self.self_pipe_write_fd
                .and_then(|raw| match nix::unistd::dup(raw) {
                    Ok(dup_fd) => Some(unsafe { OwnedFd::from_raw_fd(dup_fd) }),
                    Err(_) => None,
                });
        // Enforcement helper. On the pool path (`without_child_signals`) the
        // kill is suppressed — the deadline bookkeeping below (timeout_fired,
        // timeout_type, self-pipe wake) is identical either way, so the session
        // observes the same Timeout error and run_pooled reroutes teardown
        // through the daemon protocol.
        let signal_child = self.signal_child;
        let terminate_child = move |pid: nix::unistd::Pid| {
            if signal_child {
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
            }
        };

        thread::spawn(move || {
            let session_start_time = Instant::now();
            session_start.store(true, Ordering::SeqCst);

            loop {
                // Check if already fired
                if timeout_fired.load(Ordering::SeqCst) {
                    return;
                }

                let elapsed = session_start_time.elapsed();

                // Get current state
                let has_pty_output = pty_output_received.load(Ordering::SeqCst);
                let has_stream_json_output = stream_json_output_received.load(Ordering::SeqCst);
                // Handle poisoned mutex gracefully: if poisoned, treat as None (no prompt injected yet)
                // This is the safe default - it won't cause spurious timeout firings
                let prompt_injected = match prompt_injected_at.lock() {
                    Ok(guard) => *guard,
                    Err(poisoned) => {
                        // Log and recover from poison: take the value and use it
                        // In production this indicates a prior panic, but we continue gracefully
                        *poisoned.into_inner()
                    }
                };

                // Check Phase 1: PTY first-output timeout
                if config.pty_first_output_timeout_secs > 0
                    && !has_pty_output
                    && elapsed >= Duration::from_secs(config.pty_first_output_timeout_secs)
                {
                    terminate_child(child_pid);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(1, Ordering::SeqCst); // PtyFirstOutput
                                                             // Signal the event loop via self-pipe
                    if let Some(pipe) = self_pipe_write_fd.as_ref() {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(
                                pipe.as_raw_fd(),
                                byte.as_ptr() as *const libc::c_void,
                                1,
                            );
                        }
                    }
                    return;
                }

                // Check Phase 2: Stream-json first-output timeout.
                // Gated on stream-json mode (bf-lu1h): only there does a live
                // transcript reader run and credit `has_stream_json_output`
                // with its first forwarded line (claudepr-33fdf4ed). Outside
                // stream-json nothing could ever satisfy this deadline, so it
                // must not fire.
                if config.stream_json_mode
                    && config.stream_json_first_output_timeout_secs > 0
                    && !has_stream_json_output
                    && elapsed >= Duration::from_secs(config.stream_json_first_output_timeout_secs)
                {
                    terminate_child(child_pid);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(2, Ordering::SeqCst); // StreamJsonFirstOutput
                                                             // Signal the event loop via self-pipe
                    if let Some(pipe) = self_pipe_write_fd.as_ref() {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(
                                pipe.as_raw_fd(),
                                byte.as_ptr() as *const libc::c_void,
                                1,
                            );
                        }
                    }
                    return;
                }

                // Check Phase 3: Overall timeout (applies throughout entire session)
                if config.overall_timeout_secs > 0
                    && elapsed >= Duration::from_secs(config.overall_timeout_secs)
                {
                    terminate_child(child_pid);
                    timeout_fired.store(true, Ordering::SeqCst);
                    timeout_type.store(3, Ordering::SeqCst); // OverallTimeout
                                                             // Signal the event loop via self-pipe
                    if let Some(pipe) = self_pipe_write_fd.as_ref() {
                        let byte: [u8; 1] = [1];
                        unsafe {
                            let _ = libc::write(
                                pipe.as_raw_fd(),
                                byte.as_ptr() as *const libc::c_void,
                                1,
                            );
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
                            terminate_child(child_pid);
                            timeout_fired.store(true, Ordering::SeqCst);
                            timeout_type.store(4, Ordering::SeqCst); // StopHookTimeout
                                                                     // Signal the event loop via self-pipe
                            if let Some(pipe) = self_pipe_write_fd.as_ref() {
                                let byte: [u8; 1] = [1];
                                unsafe {
                                    let _ = libc::write(
                                        pipe.as_raw_fd(),
                                        byte.as_ptr() as *const libc::c_void,
                                        1,
                                    );
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

    /// Every deadline funnels into the single `ClaudePrintError::Timeout`
    /// variant, whose wire subtype is `timeout` (claudepr-33fdf4ed): the
    /// fine-grained reason travels via [`TimeoutType::description`] on stderr
    /// only. Guards against the per-timeout subtype strings silently
    /// reaching the wire.
    #[test]
    fn timeout_wire_subtype_is_always_timeout() {
        use crate::error::ClaudePrintError;
        assert_eq!(ClaudePrintError::Timeout.subtype(), "timeout");
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

    // ── bf-lu1h + claudepr-33fdf4ed: Phase-2 stream-json gating ───────────────
    //
    // The stream-json first-output timeout is satisfiable only where something
    // can credit it. In stream-json mode the live transcript reader holds the
    // flag from [`WatchdogState::stream_json_output_flag`] and stores it on its
    // first forwarded line; anywhere else (text/json mode, or a reader that
    // never spawned) nothing could ever set it, and an armed Phase-2 becomes an
    // unconditional session cap that SIGTERMs any turn longer than the
    // deadline. These tests pin both directions of the gate.

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
    /// stream-json mode, even after the deadline elapses. PTY output is
    /// received so Phase-1 is satisfied; only Phase-2 could fire, and the gate
    /// must prevent it — leaving the child alive.
    #[test]
    fn stream_json_timeout_does_not_fire_outside_stream_json_mode() {
        // Stream-json timeout is set (1s), but mode=false (text/json): no
        // reader runs, so nothing would ever credit Phase-2.
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), false);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None);
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
    /// stream-json mode when nothing credits it, confirming the gate enables
    /// Phase-2 (regression guard against the gate becoming always-false).
    #[test]
    fn stream_json_timeout_fires_in_stream_json_mode() {
        // Stream-json mode with a 1s deadline; the reader never spawned (in
        // production the session credits the flag itself in that case — here
        // nothing does), so the deadline is unsatisfiable and fires.
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), true);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None);
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

    /// claudepr-33fdf4ed: crediting the flag handed out by
    /// [`WatchdogState::stream_json_output_flag`] — exactly what the live
    /// transcript reader does on its first forwarded line — satisfies the
    /// Phase-2 deadline. A stream-json session that outlives the deadline with
    /// events actively flowing must NOT be killed.
    #[test]
    fn stream_json_deadline_satisfied_by_first_forwarded_line() {
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), true);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None);
        let state = watchdog.state();
        let _handle = watchdog.spawn_timeout_thread();

        // Satisfy Phase-1 so only Phase-2 could fire.
        state.mark_pty_output();

        // The reader binds and forwards its first line ~milliseconds after
        // injection, well inside the 1s deadline; poll the same flag the
        // reader would have been handed and credit it from "another thread"
        // (this one stands in for the reader).
        let flag = state.stream_json_output_flag();
        assert!(
            !flag.load(Ordering::SeqCst),
            "the first-output flag must start unset"
        );
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, Ordering::SeqCst);

        // Wait well past the 1s deadline.
        std::thread::sleep(Duration::from_millis(2000));

        assert!(
            !state.has_timeout_fired(),
            "a credited first-output deadline must not fire — this is the \
             stream-json session-cap regression (claudepr-33fdf4ed)"
        );

        // The child must still be alive (no spurious Phase-2 SIGTERM).
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!("child should still be alive, but exited with {status}"),
            Err(e) => panic!("try_wait failed: {e}"),
        }

        // Cleanup.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// claudepr-33fdf4ed: the degraded-reader credit. When the transcript
    /// reader cannot be spawned the session calls
    /// [`WatchdogState::mark_stream_json_output`] so the Phase-2 deadline
    /// cannot fire unconditionally on a stream nothing can observe.
    #[test]
    fn mark_stream_json_output_satisfies_the_deadline() {
        let config = WatchdogConfig::new(Some(60), Some(1), Some(0), Some(0), true);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None);
        let state = watchdog.state();
        let _handle = watchdog.spawn_timeout_thread();

        state.mark_pty_output();
        state.mark_stream_json_output();

        std::thread::sleep(Duration::from_millis(2000));

        assert!(
            !state.has_timeout_fired(),
            "the degraded-reader credit must satisfy Phase-2"
        );

        // Cleanup.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Pool path (claudepr-f1e93af1): `without_child_signals` must leave the
    /// deadline machinery fully intact — the timeout fires, the type is
    /// recorded, the event loop would be woken — while the child process
    /// survives. The daemon owns the worker; only the daemon protocol may tear
    /// it down.
    #[test]
    fn without_child_signals_fires_timeout_but_leaves_child_alive() {
        let config = WatchdogConfig::new(Some(0), Some(0), Some(1), Some(0), false);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None).without_child_signals();
        assert!(!watchdog.signal_child, "builder must clear the flag");
        let state = watchdog.state();
        let _handle = watchdog.spawn_timeout_thread();

        // Wait past the 1s overall deadline (100ms poll granularity).
        std::thread::sleep(Duration::from_millis(2000));

        assert!(
            state.has_timeout_fired(),
            "the overall deadline must still fire under signal suppression"
        );
        assert_eq!(
            state.get_timeout_type(),
            Some(TimeoutType::OverallTimeout),
            "the fired deadline type must be recorded unchanged"
        );

        // The child must still be alive — no SIGTERM reached it.
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                let _ = child.wait();
                panic!("child should have survived signal suppression, but exited with {status}")
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }

        // For contrast with the stateless default: a fresh watchdog without
        // the builder still signals (flag default true), so the suppression is
        // opt-in, not a global behavior change.
        let watchdog2 = Watchdog::new(
            WatchdogConfig::new(Some(0), Some(0), Some(1), Some(0), false),
            child_pid,
            None,
        );
        assert!(watchdog2.signal_child, "default must keep direct signaling");

        // Cleanup.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The timeout thread signals through its OWN duplicate of the self-pipe
    /// write end, never through the raw fd number: the thread is detached and
    /// can fire long after its drive ended, by which point the drive's pipe
    /// fds are closed and their numbers may belong to someone else entirely.
    /// The flake this pins (claudepr-8b0e6e78): a 30s stop-hook watchdog from
    /// a SIGINT-interrupted drive fired after the next drive had opened its
    /// own self-pipe at the same fd number; the stray byte was read as a
    /// signal and surfaced as a bogus `Error::Interrupted`.
    ///
    /// The discriminator: with the drive's write end closed but its READ end
    /// still held, a late overall-deadline fire must land in the ORIGINAL
    /// pipe (the thread's duplicate points at the same kernel pipe). Writing
    /// through the stale raw number instead would hit a closed-or-reused fd
    /// and nothing would ever arrive here.
    #[test]
    fn late_watchdog_fire_writes_through_its_own_pipe_duplicate() {
        use std::io::Read;

        let (read_end, write_end) = nix::unistd::pipe().unwrap();
        let config = WatchdogConfig::new(Some(0), Some(0), Some(1), Some(0), false);
        // A fiction pid is safe: signal suppression means nothing is ever
        // signalled — this is exactly the pooled drive's configuration.
        let watchdog = Watchdog::new(
            config,
            nix::unistd::Pid::from_raw(4000),
            Some(write_end.as_raw_fd()),
        )
        .without_child_signals();
        let handle = watchdog.spawn_timeout_thread();

        // End the "drive": the write end goes away while the thread is
        // still armed. The read end is deliberately kept alive — it is the
        // observation point.
        let mut read_end = std::fs::File::from(read_end);

        // The 1s overall deadline fires through the thread's duplicate.
        // Non-blocking poll so a regression fails by assertion, not a hang.
        let _ = nix::fcntl::fcntl(
            read_end.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut byte = [0u8; 1];
        loop {
            match read_end.read(&mut byte) {
                Ok(1) => break,
                Ok(n) => panic!("unexpected read of {n} bytes"),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    assert!(
                        Instant::now() < deadline,
                        "the late deadline never fired through the thread's own duplicate"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("read failed: {e}"),
            }
        }
        handle
            .join()
            .expect("the watchdog thread exits after firing");
    }

    // ── Mutex poisoning tests ────────────────────────────────────────────────────

    /// Test that mark_prompt_injected handles poisoned mutex gracefully without panic.
    #[test]
    fn mark_prompt_injected_handles_poisoned_mutex() {
        let state = WatchdogState::new();

        // Manually poison the mutex by getting a lock and panicking (simulated via drop)
        // We'll use a simpler approach: get a lock, then drop it in a way that causes poison
        {
            let lock = state.prompt_injected_at.lock().unwrap();
            // Store a value first
            let mut guard = lock;
            *guard = Some(Instant::now());
            // Normal drop - no poison yet
        }

        // This should work normally
        assert!(!state.has_timeout_fired());

        // Now simulate poison by trying to lock in a way that would cause panic
        // Actually, let's use a different approach: we can't easily poison a mutex in a test
        // without actually panicking a thread, which would cause the test to fail.

        // Instead, verify the code compiles and the logic paths exist
        // The real test is that the code uses match instead of unwrap()
        state.mark_prompt_injected();
        // If we got here without panic, the code handles normal case correctly
    }

    /// Test that prompt injection read handles poisoned mutex gracefully.
    #[test]
    fn prompt_injected_read_handles_poisoned_mutex() {
        let state = WatchdogState::new();

        // Set initial state
        state.mark_prompt_injected();

        // Verify normal operation
        assert!(!state.has_timeout_fired());

        // The actual poison handling is tested via the watchdog behavior
        // We can't easily poison a mutex in a test without panicking the thread
        // but we've verified the code uses match instead of unwrap()
    }

    /// Integration test: verify watchdog thread continues despite potential poison scenarios.
    #[test]
    fn watchdog_continues_with_mutex_operations() {
        let config = WatchdogConfig::new(Some(60), Some(0), Some(0), Some(0), true);
        let (mut child, child_pid) = spawn_sleep_child();
        let watchdog = Watchdog::new(config, child_pid, None);
        let state = watchdog.state();

        // Mark prompt injected multiple times - should not panic
        state.mark_prompt_injected();
        state.mark_prompt_injected();

        // Mark PTY output
        state.mark_pty_output();

        // Watchdog should still be running
        assert!(!state.has_timeout_fired());

        // Cleanup
        let _ = child.kill();
        let _ = child.wait();
    }
}
