//! Error types for the provenance hash chain (Issue #3351).

use thiserror::Error;

/// Result alias for provenance-chain operations.
pub type ChainResult<T> = Result<T, ChainError>;

/// Errors produced by the provenance hash chain store and codecs.
#[derive(Debug, Error)]
pub enum ChainError {
    /// An underlying I/O operation failed.
    #[error("provenance chain I/O error: {0}")]
    Io(String),

    /// The on-disk data is structurally invalid (bad magic, unexpected EOF in a
    /// framed field, impossible length, etc.).
    #[error("provenance chain corruption: {0}")]
    Corrupt(String),

    /// The file header magic or format version did not match what this build
    /// understands.
    #[error("provenance chain header mismatch: {0}")]
    HeaderMismatch(String),

    /// A length-prefixed record could not be decoded because its framing was
    /// inconsistent (declared length disagrees with available bytes).
    #[error("provenance chain framing error: {0}")]
    BadFraming(String),
}

impl From<std::io::Error> for ChainError {
    fn from(e: std::io::Error) -> Self {
        ChainError::Io(e.to_string())
    }
}
