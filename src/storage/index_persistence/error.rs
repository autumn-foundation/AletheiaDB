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

    #[test]
    fn test_error_formatting() {
        let e1 = IndexPersistenceError::InternerMismatch {
            expected: 1,
            got: 2,
        };
        assert_eq!(
            e1.to_string(),
            "String interner mismatch: expected index 1, got 2"
        );

        let e2 = IndexPersistenceError::UnsupportedVersion {
            found: 2,
            supported: 1,
        };
        assert_eq!(
            e2.to_string(),
            "Manifest version 2 not supported (max supported: 1)"
        );

        let e3 = IndexPersistenceError::MissingIndex {
            name: "test".to_string(),
        };
        assert_eq!(e3.to_string(), "Missing required index file: test");

        let e4 = IndexPersistenceError::InvalidMagic {
            path: "test".into(),
            expected: [1, 2, 3, 4],
            got: [4, 3, 2, 1],
        };
        assert_eq!(
            e4.to_string(),
            "Invalid magic bytes in test: expected [1, 2, 3, 4], got [4, 3, 2, 1]"
        );

        let e5 = IndexPersistenceError::SizeLimitExceeded {
            message: "test".to_string(),
        };
        assert_eq!(e5.to_string(), "Size limit exceeded: test");

        let e6 = IndexPersistenceError::Serialization("bitcode error".to_string());
        assert_eq!(e6.to_string(), "Serialization error: bitcode error");

        let e7 = IndexPersistenceError::Corrupted {
            path: "test".into(),
            source: Box::new(std::io::Error::other("io error")),
        };
        assert_eq!(e7.to_string(), "Index file corrupted: test");
    }

    #[test]
    fn test_is_not_found() {
        let e_io_not_found =
            IndexPersistenceError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "test"));
        assert!(e_io_not_found.is_not_found());

        let e_io_other = IndexPersistenceError::Io(std::io::Error::other("test"));
        assert!(!e_io_other.is_not_found());

        let e_other = IndexPersistenceError::SizeLimitExceeded {
            message: "test".to_string(),
        };
        assert!(!e_other.is_not_found());
    }

    #[test]
    fn test_from_bitcode_error() {
        // Trigger a real bitcode error by attempting to deserialize invalid bytes
        let invalid_bytes: &[u8] = &[0xFF, 0xFF, 0xFF];
        let bitcode_result: std::result::Result<String, bitcode::Error> =
            bitcode::decode(invalid_bytes);
        assert!(bitcode_result.is_err());

        let bitcode_err = bitcode_result.unwrap_err();
        let persistence_err: IndexPersistenceError = bitcode_err.into();

        assert!(matches!(
            persistence_err,
            IndexPersistenceError::Serialization(_)
        ));
        assert!(
            persistence_err
                .to_string()
                .starts_with("Serialization error:")
        );
    }
}
