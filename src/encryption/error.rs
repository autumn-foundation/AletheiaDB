//! Error types for encryption operations.

use thiserror::Error;

/// Errors from encryption/decryption operations.
#[derive(Debug, Error)]
pub enum EncryptionError {
    /// Encryption operation failed.
    #[error("Encryption failed: {0}")]
    EncryptFailed(String),

    /// Decryption operation failed (wrong key, corrupted data, or tampered).
    #[error("Decryption failed: {0}")]
    DecryptFailed(String),

    /// Nonce generation failed.
    #[error("Nonce generation failed: {0}")]
    NonceError(String),

    /// Invalid ciphertext length (too short for nonce + tag).
    #[error("Invalid ciphertext: expected at least {expected} bytes, got {actual}")]
    InvalidCiphertext {
        /// Minimum expected length.
        expected: usize,
        /// Actual length received.
        actual: usize,
    },

    /// Serialized WAL entry is too short to contain the required header.
    #[error("Invalid WAL entry: expected at least {expected} bytes, got {actual}")]
    InvalidWalEntry {
        /// Minimum expected length (25 bytes: 24 header + 1 op type).
        expected: usize,
        /// Actual length received.
        actual: usize,
    },
}

/// Errors from key provider operations.
#[derive(Debug, Error)]
pub enum KeyProviderError {
    /// Key not found in the provider.
    #[error("Key not found")]
    KeyNotFound,

    /// Access to the key was denied.
    #[error("Access denied: {0}")]
    AccessDenied(String),

    /// Key provider is unavailable.
    #[error("Provider unavailable: {0}")]
    Unavailable(String),

    /// Key data has invalid format.
    #[error("Invalid key format: {0}")]
    InvalidKeyFormat(String),

    /// I/O error reading key material.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic provider error.
    #[error("Provider error: {0}")]
    Provider(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Errors from key derivation operations.
#[derive(Debug, Error)]
pub enum KeyDerivationError {
    /// HKDF expansion failed (should never happen with valid params).
    #[error("Key derivation failed: {0}")]
    DerivationFailed(String),

    /// Master key has invalid length.
    #[error("Invalid master key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_error_display() {
        let err = EncryptionError::EncryptFailed("bad data".into());
        assert!(err.to_string().contains("bad data"));
    }

    #[test]
    fn encryption_error_invalid_ciphertext() {
        let err = EncryptionError::InvalidCiphertext {
            expected: 28,
            actual: 10,
        };
        assert!(err.to_string().contains("28"));
        assert!(err.to_string().contains("10"));
    }

    #[test]
    fn key_provider_error_display() {
        let err = KeyProviderError::KeyNotFound;
        assert_eq!(err.to_string(), "Key not found");
    }

    #[test]
    fn key_provider_error_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let err = KeyProviderError::from(io_err);
        assert!(err.to_string().contains("file gone"));
    }

    #[test]
    fn key_derivation_error_display() {
        let err = KeyDerivationError::InvalidKeyLength(16);
        assert!(err.to_string().contains("16"));
    }
}
