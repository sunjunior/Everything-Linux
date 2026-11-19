use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("snapshot error: {0}")]
    Snapshot(String),

    #[error("query parse error: {0}")]
    Query(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("root not found: {0}")]
    RootNotFound(String),

    #[error("daemon error: {0}")]
    Daemon(String),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
