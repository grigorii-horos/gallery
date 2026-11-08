/// Errors produced by gallery-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("tool: {0}")]
    Tool(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("syncthing: {0}")]
    Sync(String),
    #[error("ai: {0}")]
    Ai(String),
    #[error("plan: {0}")]
    Plan(String),
    /// exiftool cannot write this file type at all (mp3, ogg, txt, ...).
    #[error("exiftool cannot write: {0}")]
    Unwritable(String),
    /// The operation was cancelled (see `progress::Cancel`).
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;
