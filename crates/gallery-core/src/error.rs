/// Errors produced by gallery-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}", crate::t!("error.config", detail = .0))]
    Config(String),
    #[error("{}", crate::t!("error.tool", detail = .0))]
    Tool(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{}", crate::t!("error.syncthing", detail = .0))]
    Sync(String),
    #[error("{}", crate::t!("error.ai", detail = .0))]
    Ai(String),
    #[error("{}", crate::t!("error.plan", detail = .0))]
    Plan(String),
    /// exiftool cannot write this file type at all (mp3, ogg, txt, ...).
    #[error("{}", crate::t!("error.unwritable", detail = .0))]
    Unwritable(String),
    /// The operation was cancelled (see `progress::Cancel`).
    #[error("{}", crate::t!("error.cancelled"))]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;
