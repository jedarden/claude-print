use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// User-facing error type with exit code and JSON subtype mapping.
#[derive(Debug)]
pub enum ClaudePrintError {
    Setup(String),          // exit 2
    Timeout,                // exit 124
    Interrupted,            // exit 130
    AssistantError(String), // exit 1
}

impl ClaudePrintError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ClaudePrintError::Setup(_) => 2,
            ClaudePrintError::Timeout => 124,
            ClaudePrintError::Interrupted => 130,
            ClaudePrintError::AssistantError(_) => 1,
        }
    }

    pub fn subtype(&self) -> &'static str {
        match self {
            ClaudePrintError::Setup(_) => "internal_error",
            ClaudePrintError::Timeout => "timeout",
            ClaudePrintError::Interrupted => "interrupted",
            ClaudePrintError::AssistantError(_) => "assistant_error",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ClaudePrintError::Setup(m) => m,
            ClaudePrintError::Timeout => "operation timed out",
            ClaudePrintError::Interrupted => "interrupted by signal",
            ClaudePrintError::AssistantError(m) => m,
        }
    }
}
