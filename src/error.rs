//! Comprehensive error types for claude-print.
//!
//! This module defines:
//! - [`Error`]: Internal error enum for library-level error handling
//! - [`Result`]: Type alias for `Result<T, Error>`
//! - [`ClaudePrintError`]: User-facing error type with exit code and JSON subtype mapping

use thiserror::Error;

/// Internal error type for library-level operations.
///
/// These errors are converted to [`ClaudePrintError`] before being presented
/// to the user. The conversion ensures appropriate exit codes and user-friendly
/// messages.
#[derive(Debug, Error)]
pub enum Error {
    /// Generic internal error - wraps anyhow::Error for flexible error context.
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),

    /// I/O error - wraps std::io::Error for filesystem and pipe operations.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration file error - malformed or missing config values.
    #[error("invalid config: {0}")]
    Config(String),

    /// PTY spawn failure - openpty(3) failed to allocate a pseudo-terminal.
    #[error("failed to open PTY: {0}")]
    OpenptyFailed(String),

    /// Fork failure - fork(2) failed to create a child process.
    #[error("failed to fork: {0}")]
    ForkFailed(String),

    /// Signal handler installation failure - signal(2) failed.
    #[error("failed to install signal handler: {0}")]
    SignalHandlerFailed(String),

    /// Waitpid failure - waitpid(2) failed to reap child exit status.
    #[error("waitpid failed: {0}")]
    WaitpidFailed(String),

    /// Hook setup error - failed to create temp dir, FIFO, or hook files.
    #[error("hook setup failed: {0}")]
    HookSetupFailed(String),

    /// FIFO operation error - opening or reading from the Stop hook FIFO.
    #[error("FIFO error: {0}")]
    FifoError(String),

    /// Parse error - failed to deserialize JSON (transcript or stop payload).
    #[error("parse error: {0}")]
    ParseFailed(String),

    /// Timeout error - operation exceeded deadline.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Interruption error - operation aborted by signal (SIGINT, SIGTERM, etc.).
    #[error("interrupted: {0}")]
    Interrupted(String),

    /// Binary resolution error - failed to locate or validate claude binary.
    #[error("claude binary error: {0}")]
    BinaryResolutionFailed(String),

    /// Version compatibility error - claude version check failed.
    #[error("version check failed: {0}")]
    VersionCheckFailed(String),

    /// Terminal probe error - failed to parse terminal capabilities.
    #[error("terminal probe error: {0}")]
    TerminalError(String),

    /// Child process error - child exited with unexpected status.
    #[error("child process error: {0}")]
    ChildProcessError(String),

    /// Assistant error - Claude Code's own transcript result event reported
    /// `is_error: true` (rate limit, tool failure, or any assistant-side error).
    /// Maps to exit code 1, distinct from Setup (2) which is a claude-print failure.
    #[error("assistant error: {0}")]
    AssistantError(String),
}

/// Result type alias for operations that can fail with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// User-facing error type with exit code and JSON subtype mapping.
///
/// This is the error type presented to users via CLI output or JSON results.
/// Each variant maps to a specific exit code and JSON subtype for structured
/// error reporting.
#[derive(Debug)]
pub enum ClaudePrintError {
    /// Setup failure - missing prerequisites, binary not found, etc. (exit 2)
    Setup(String),

    /// Configuration failure detected before a session starts (exit 2).
    Config(String),

    /// Timeout - operation exceeded deadline (exit 124, matching GNU timeout).
    Timeout,

    /// Interrupted - caught SIGINT/SIGTERM (exit 130, matching Git behavior).
    Interrupted,

    /// Assistant error - Claude Code returned an error result (exit 1).
    AssistantError(String),
}

impl ClaudePrintError {
    /// Returns the appropriate exit code for this error.
    ///
    /// - Setup: 2 (similar to bash's misuse of shell builtins)
    /// - Timeout: 124 (GNU timeout convention)
    /// - Interrupted: 128 + 2 (128 + SIGINT, Git convention)
    /// - AssistantError: 1 (generic error)
    pub fn exit_code(&self) -> i32 {
        match self {
            ClaudePrintError::Setup(_) | ClaudePrintError::Config(_) => 2,
            ClaudePrintError::Timeout => 124,
            ClaudePrintError::Interrupted => 130,
            ClaudePrintError::AssistantError(_) => 1,
        }
    }

    /// Returns the JSON subtype for this error (used in JSON/stream-json output modes).
    pub fn subtype(&self) -> &'static str {
        match self {
            ClaudePrintError::Setup(_) | ClaudePrintError::Config(_) => "internal_error",
            ClaudePrintError::Timeout => "timeout",
            ClaudePrintError::Interrupted => "interrupted",
            ClaudePrintError::AssistantError(_) => "assistant_error",
        }
    }

    /// Returns the human-readable error message.
    pub fn message(&self) -> &str {
        match self {
            ClaudePrintError::Setup(m) | ClaudePrintError::Config(m) => m,
            ClaudePrintError::Timeout => "operation timed out",
            ClaudePrintError::Interrupted => "interrupted by signal",
            ClaudePrintError::AssistantError(m) => m,
        }
    }
}

impl From<Error> for ClaudePrintError {
    /// Converts internal [`Error`] to user-facing [`ClaudePrintError`].
    ///
    /// This conversion maps technical errors to appropriate user-facing
    /// categories with clear messages.
    fn from(err: Error) -> Self {
        match err {
            Error::Timeout(_) => ClaudePrintError::Timeout,
            Error::Interrupted(_) => ClaudePrintError::Interrupted,
            Error::AssistantError(msg) => ClaudePrintError::AssistantError(msg),
            Error::Config(message) => {
                let message = if let Some(detail) = message.strip_prefix("invalid config at ") {
                    format!("invalid config: {detail}")
                } else if message.starts_with("invalid config:") {
                    message
                } else {
                    format!("invalid config: {message}")
                };
                ClaudePrintError::Config(message)
            }
            other => ClaudePrintError::Setup(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_formats_correctly() {
        let err = Error::OpenptyFailed("no ptys available".to_string());
        assert!(err.to_string().contains("open PTY"));
        assert!(err.to_string().contains("no ptys available"));
    }

    #[test]
    fn fork_failed_error_display() {
        let err = Error::ForkFailed("resource temporarily unavailable".to_string());
        assert!(err.to_string().contains("fork"));
        assert!(err.to_string().contains("resource temporarily unavailable"));
    }

    #[test]
    fn signal_handler_error_display() {
        let err = Error::SignalHandlerFailed("SIGWINCH: operation not permitted".to_string());
        assert!(err.to_string().contains("signal handler"));
        assert!(err.to_string().contains("SIGWINCH"));
    }

    #[test]
    fn waitpid_failed_error_display() {
        let err = Error::WaitpidFailed("no child process".to_string());
        assert!(err.to_string().contains("waitpid"));
    }

    #[test]
    fn hook_setup_error_display() {
        let err = Error::HookSetupFailed("mkfifo failed: permission denied".to_string());
        assert!(err.to_string().contains("hook setup"));
        assert!(err.to_string().contains("permission denied"));
    }

    #[test]
    fn parse_error_display() {
        let err = Error::ParseFailed("invalid JSON: unexpected token".to_string());
        assert!(err.to_string().contains("parse error"));
    }

    #[test]
    fn timeout_error_display() {
        let err = Error::Timeout("startup phase exceeded 45s deadline".to_string());
        assert!(err.to_string().contains("timeout"));
        assert!(err.to_string().contains("45s"));
    }

    #[test]
    fn interrupted_error_display() {
        let err = Error::Interrupted("SIGINT received".to_string());
        assert!(err.to_string().contains("interrupted"));
        assert!(err.to_string().contains("SIGINT"));
    }

    #[test]
    fn binary_resolution_error_display() {
        let err = Error::BinaryResolutionFailed("claude not found in PATH".to_string());
        assert!(err.to_string().contains("claude binary"));
    }

    #[test]
    fn claude_print_error_setup() {
        let err = ClaudePrintError::Setup("claude binary not found".to_string());
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.subtype(), "internal_error");
        assert_eq!(err.message(), "claude binary not found");
    }

    #[test]
    fn claude_print_error_timeout() {
        let err = ClaudePrintError::Timeout;
        assert_eq!(err.exit_code(), 124);
        assert_eq!(err.subtype(), "timeout");
        assert_eq!(err.message(), "operation timed out");
    }

    #[test]
    fn claude_print_error_interrupted() {
        let err = ClaudePrintError::Interrupted;
        assert_eq!(err.exit_code(), 130);
        assert_eq!(err.subtype(), "interrupted");
        assert_eq!(err.message(), "interrupted by signal");
    }

    #[test]
    fn claude_print_error_assistant_error() {
        let err = ClaudePrintError::AssistantError("tool execution failed".to_string());
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.subtype(), "assistant_error");
        assert_eq!(err.message(), "tool execution failed");
    }

    #[test]
    fn error_to_claude_print_error_timeout() {
        let internal = Error::Timeout("deadline exceeded".to_string());
        let user_facing: ClaudePrintError = internal.into();
        assert!(matches!(user_facing, ClaudePrintError::Timeout));
    }

    #[test]
    fn error_to_claude_print_error_interrupted() {
        let internal = Error::Interrupted("SIGTERM".to_string());
        let user_facing: ClaudePrintError = internal.into();
        assert!(matches!(user_facing, ClaudePrintError::Interrupted));
    }

    #[test]
    fn error_to_claude_print_error_setup() {
        let internal = Error::BinaryResolutionFailed("not found".to_string());
        let user_facing: ClaudePrintError = internal.into();
        match user_facing {
            ClaudePrintError::Setup(msg) => {
                assert!(msg.contains("not found"));
            }
            _ => panic!("expected Setup variant"),
        }
    }

    #[test]
    fn error_to_claude_print_error_assistant_error() {
        // An assistant-side error (is_error:true transcript) must map to
        // exit-1 AssistantError, NOT the Setup catch-all (exit 2).
        let internal = Error::AssistantError("rate limit exceeded".to_string());
        let user_facing: ClaudePrintError = internal.into();
        // Match on a reference so `user_facing` is not partially moved and can
        // still be queried for exit_code/subtype below.
        match &user_facing {
            ClaudePrintError::AssistantError(msg) => {
                assert_eq!(msg.as_str(), "rate limit exceeded");
            }
            _ => panic!("expected AssistantError variant"),
        }
        assert_eq!(user_facing.exit_code(), 1);
        assert_eq!(user_facing.subtype(), "assistant_error");
    }

    #[test]
    fn config_error_converts_to_typed_startup_error() {
        let internal = Error::Config("invalid config at /tmp/config.toml: bad TOML".to_string());
        let user_facing: ClaudePrintError = internal.into();

        assert!(matches!(user_facing, ClaudePrintError::Config(_)));
        assert_eq!(user_facing.exit_code(), 2);
        assert_eq!(user_facing.subtype(), "internal_error");
        assert_eq!(
            user_facing.message(),
            "invalid config: /tmp/config.toml: bad TOML"
        );
    }

    #[test]
    fn io_error_converts_to_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn internal_error_converts_to_error() {
        let anyhow_err = anyhow::anyhow!("something went wrong");
        let err: Error = anyhow_err.into();
        assert!(matches!(err, Error::Internal(_)));
        assert!(err.to_string().contains("something went wrong"));
    }
}
