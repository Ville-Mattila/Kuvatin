pub mod batch;
pub mod crop;
pub mod format;
pub mod heif;
pub mod menu;
pub mod metadata;
pub mod naming;
pub mod pipeline;
pub mod preset;
pub mod resize;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("failed to read image {path}: {source}")]
    Decode {
        path: PathBuf,
        source: image::ImageError,
    },
    #[error("failed to encode image: {0}")]
    Encode(String),
    #[error("io error for {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid job: {0}")]
    InvalidJob(String),
    /// An image of a kind Kuvatin knows that could not be read here (a HEIC
    /// without Windows' codecs); the message is complete as it stands.
    #[error("{0}")]
    Unreadable(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
