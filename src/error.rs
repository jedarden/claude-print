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
