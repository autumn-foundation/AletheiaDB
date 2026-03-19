//! Error types for index persistence operations.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during index persistence operations.
#[derive(Debug, Error)]
pub enum IndexPersistenceError {
    /// Index file is corrupted or invalid
    #[error("Index file corrupted: {path}")]
    Corrupted {
        /// Path to the corrupted file
        path: PathBuf,
        #[source]
        /// Source error
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// String interner index mismatch during restoration
    #[error("String interner mismatch: expected index {expected}, got {got}")]
    InternerMismatch {
        /// Expected index value
        expected: u32,
        /// Actual index value received
        got: u32,
    },

    /// Manifest version not supported
    #[error("Manifest version {found} not supported (max supported: {supported})")]
    UnsupportedVersion {
        /// Version found in manifest
        found: u16,
        /// Maximum supported version
        supported: u16,
    },

    /// Required index file is missing
    #[error("Missing required index file: {name}")]
    MissingIndex {
        /// Name of missing index file
        name: String,
    },

    /// Invalid magic bytes in file header
    #[error("Invalid magic bytes in {path}: expected {expected:?}, got {got:?}")]
    InvalidMagic {
        /// Path to file with invalid magic bytes
        path: PathBuf,
        /// Expected magic bytes
        expected: [u8; 4],
        /// Actual magic bytes found
        got: [u8; 4],
    },

    /// Size limit exceeded (DoS protection)
    #[error("Size limit exceeded: {message}")]
    SizeLimitExceeded {
        /// Description of the size limit violation
        message: String,
    },

    /// IO error during persistence operations
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Bitcode serialization/deserialization error
    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<bitcode::Error> for IndexPersistenceError {
    fn from(e: bitcode::Error) -> Self {
        IndexPersistenceError::Serialization(e.to_string())
    }
}

impl IndexPersistenceError {
    /// Check if this error is due to a file not being found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            IndexPersistenceError::Io(e) if e.kind() == std::io::ErrorKind::NotFound
        )
    }
}

/// Result type for index persistence operations.
pub type Result<T> = std::result::Result<T, IndexPersistenceError>;
#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn should_return_true_when_error_is_not_found() {
        // Test with NotFound error
        let err =
            IndexPersistenceError::Io(io::Error::new(io::ErrorKind::NotFound, "file missing"));
        assert!(
            err.is_not_found(),
            "is_not_found() should return true for ErrorKind::NotFound"
        );

        // Test with other IO error
        let err2 =
            IndexPersistenceError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        assert!(
            !err2.is_not_found(),
            "is_not_found() should return false for other IO errors"
        );

        // Test with non-IO error
        let err3 = IndexPersistenceError::MissingIndex {
            name: "test".to_string(),
        };
        assert!(
            !err3.is_not_found(),
            "is_not_found() should return false for non-IO errors"
        );
    }

    #[test]
    fn should_convert_from_bitcode_error() {
        let bad_data = vec![0xFF, 0xFF, 0xFF];
        let bitcode_err = bitcode::decode::<u32>(&bad_data).unwrap_err();

        let err: IndexPersistenceError = bitcode_err.into();
        assert!(
            matches!(err, IndexPersistenceError::Serialization(_)),
            "Should wrap bitcode::Error in Serialization variant"
        );
    }
}
