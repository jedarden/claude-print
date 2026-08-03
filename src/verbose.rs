//! `--verbose` timing traces (plan §"`--verbose` Trace Points`").
//!
//! When `--verbose` is passed, claude-print writes timestamped
//! `[claude-print <ms>ms] <message>` lines to stderr — never stdout — covering
//! the session lifecycle: temp dir created, PTY opened, child forked (pid),
//! phase transitions, FIFO opened, prompt injected, Stop received (session id),
//! transcript retry count, and cleanup reason.
//!
//! When `--verbose` is off (the default), every method is a cheap no-op so the
//! flag costs nothing on the hot path: a single `enabled` branch test, no I/O,
//! no allocation.
//!
//! The `<ms>` timestamp is milliseconds since the tracer's start instant, which
//! is the session start captured at the top of
//! [`crate::session::Session::run`]. This is the same clock the success path
//! uses for `duration_ms`, so trace timestamps line up with reported timing.

use std::io::Write;
use std::time::Instant;

/// Emits `[claude-print <ms>ms] <message>` traces to stderr when enabled.
///
/// Constructed once at the start of the session from the `--verbose` flag and
/// the session-start instant, then cloned cheaply into the event-loop closure
/// and threaded into the transcript reader. `enabled` makes every call a cheap
/// early return when `--verbose` is off.
#[derive(Clone, Copy)]
pub struct Tracer {
    enabled: bool,
    start: Instant,
}

impl Tracer {
    /// Create a tracer. `enabled` mirrors `cli.verbose`; `start` is the
    /// session-start instant captured at the top of `run_inner`.
    pub fn new(enabled: bool, start: Instant) -> Self {
        Self { enabled, start }
    }

    /// A disabled tracer — every call is a no-op.
    ///
    /// Used by callers that never run a session (e.g. the untraced
    /// [`crate::transcript::read_transcript`] entry point exercised in unit
    /// tests) and as a convenient default.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            // The start instant is irrelevant when disabled; pick one cheaply.
            start: Instant::now(),
        }
    }

    /// True when `--verbose` was passed and [`Tracer::trace`] will emit.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Emit `[claude-print <ms>ms] <message>` to stderr. No-op when disabled.
    ///
    /// Best-effort: a failed stderr write never fails the session.
    pub fn trace(&self, message: impl std::fmt::Display) {
        if !self.enabled {
            return;
        }
        let ms = self.start.elapsed().as_millis();
        // Ignore errors: a closed stderr must not abort the run.
        let _ = writeln!(std::io::stderr(), "[claude-print {}ms] {}", ms, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_tracer_reports_disabled_and_no_ops() {
        let t = Tracer::disabled();
        assert!(!t.is_enabled());
        // Must not panic; output goes to the test's stderr and is ignored.
        t.trace("this line should be a no-op (not emitted)");
    }

    #[test]
    fn enabled_tracer_reports_enabled() {
        let t = Tracer::new(true, Instant::now());
        assert!(t.is_enabled());
    }

    #[test]
    fn tracer_is_copy_and_clone() {
        // Threaded into closures and the transcript reader via Clone/Copy; the
        // derive must hold so the no-op path stays allocation-free.
        let t = Tracer::new(false, Instant::now());
        let _copy = t;
        let _clone = t;
    }
}
