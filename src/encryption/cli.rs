//! CLI helper functions for encryption key management.
//!
//! These functions implement the logic for encryption CLI commands:
//! - `keys generate` -- Generate a new master key file
//! - `keys info` -- Display current key information
//! - `encryption status` -- Show encryption status for all layers
//! - `encryption verify` -- Verify encrypted data is readable

use std::path::Path;

use crate::encryption::KeyProviderError;
use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
use crate::encryption::factory::Algorithm;
use crate::encryption::key_provider::{FileKeyProvider, KeyProvider};

/// Result of a key generation operation.
#[derive(Debug)]
pub struct KeyGenResult {
    /// Path where the key was written.
    pub path: String,
    /// Algorithm the key is for.
    pub algorithm: String,
    /// Key length in bytes.
    pub key_length: usize,
}

/// Generate a new master key file.
///
/// Creates a cryptographically random 256-bit key at the given path, encoded as
/// 64 hex characters. Parent directories are created automatically.
///
/// # Errors
///
/// Returns [`KeyProviderError`] if directory creation or file writing fails.
pub fn generate_key(output_path: &Path) -> Result<KeyGenResult, KeyProviderError> {
    let _key = FileKeyProvider::generate_key_file(output_path)?;
    Ok(KeyGenResult {
        path: output_path.display().to_string(),
        algorithm: "AES-256-GCM / ChaCha20-Poly1305".into(),
        key_length: 32,
    })
}

/// Information about the current encryption configuration.
#[derive(Debug)]
pub struct EncryptionStatus {
    /// Whether encryption is enabled.
    pub enabled: bool,
    /// Algorithm in use (if enabled).
    pub algorithm: Option<String>,
    /// Key provider type (if enabled).
    pub provider_type: Option<String>,
    /// Key provider detail (path or variable name).
    pub provider_detail: Option<String>,
}

/// Get encryption status from configuration.
///
/// Extracts human-readable status information from an [`EncryptionConfig`].
#[must_use]
pub fn get_encryption_status(config: &EncryptionConfig) -> EncryptionStatus {
    if !config.enabled {
        return EncryptionStatus {
            enabled: false,
            algorithm: None,
            provider_type: None,
            provider_detail: None,
        };
    }

    let algorithm = match config.algorithm {
        Algorithm::Aes256Gcm => "AES-256-GCM",
        Algorithm::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        Algorithm::Auto => "Auto (AES-256-GCM if AES-NI, else ChaCha20-Poly1305)",
    };

    let (provider_type, provider_detail) = match &config.key_provider {
        KeyProviderConfig::File { path } => ("file", path.display().to_string()),
        KeyProviderConfig::Env { variable } => ("env", variable.clone()),
    };

    EncryptionStatus {
        enabled: true,
        algorithm: Some(algorithm.into()),
        provider_type: Some(provider_type.into()),
        provider_detail: Some(provider_detail),
    }
}

/// Validate that a key file exists and is readable.
///
/// Attempts to load and parse the key from the given path. Returns `Ok(())`
/// if the key is valid, or an error describing the problem.
///
/// # Errors
///
/// Returns [`KeyProviderError`] if the file does not exist, cannot be read,
/// or contains an invalid key format.
pub fn validate_key_file(path: &Path) -> Result<(), KeyProviderError> {
    let provider = FileKeyProvider::new(path);
    provider.health_check()
}

/// Display encryption status as formatted text.
#[must_use]
pub fn format_encryption_status(status: &EncryptionStatus) -> String {
    let mut out = String::new();
    out.push_str("Encryption Status\n");
    // U+2500 BOX DRAWINGS LIGHT HORIZONTAL
    out.push_str("\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n");

    if !status.enabled {
        out.push_str("Overall:        DISABLED\n");
        return out;
    }

    out.push_str("Overall:        ENABLED\n");
    if let Some(ref alg) = status.algorithm {
        out.push_str(&format!("Algorithm:      {alg}\n"));
    }
    if let Some(ref pt) = status.provider_type {
        out.push_str(&format!("Key Provider:   {pt}"));
        if let Some(ref pd) = status.provider_detail {
            out.push_str(&format!(" ({pd})"));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn generate_key_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");

        let result = generate_key(&key_path).unwrap();

        assert!(key_path.exists(), "key file should exist after generation");
        assert_eq!(result.path, key_path.display().to_string());
    }

    #[test]
    fn generate_key_result_correct() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("result_test.key");

        let result = generate_key(&key_path).unwrap();

        assert_eq!(result.key_length, 32);
        assert!(result.algorithm.contains("AES-256-GCM"));
        assert!(result.algorithm.contains("ChaCha20-Poly1305"));
    }

    #[test]
    fn get_status_disabled() {
        let config = EncryptionConfig::disabled();
        let status = get_encryption_status(&config);

        assert!(!status.enabled);
        assert!(status.algorithm.is_none());
        assert!(status.provider_type.is_none());
        assert!(status.provider_detail.is_none());
    }

    #[test]
    fn get_status_enabled_file() {
        let config = EncryptionConfig::file_based("/tmp/my.key");
        let status = get_encryption_status(&config);

        assert!(status.enabled);
        assert!(status.algorithm.is_some());
        assert_eq!(status.provider_type.as_deref(), Some("file"));
        assert!(status.provider_detail.as_ref().unwrap().contains("my.key"));
    }

    #[test]
    fn get_status_enabled_env() {
        let config = EncryptionConfig::env_based("MY_SECRET_KEY");
        let status = get_encryption_status(&config);

        assert!(status.enabled);
        assert!(status.algorithm.is_some());
        assert_eq!(status.provider_type.as_deref(), Some("env"));
        assert_eq!(status.provider_detail.as_deref(), Some("MY_SECRET_KEY"));
    }

    #[test]
    fn get_status_algorithm_variants() {
        // Auto
        let config = EncryptionConfig {
            enabled: true,
            algorithm: Algorithm::Auto,
            key_provider: KeyProviderConfig::Env {
                variable: "X".into(),
            },
        };
        let status = get_encryption_status(&config);
        assert!(status.algorithm.as_ref().unwrap().contains("Auto"));

        // AES
        let config = EncryptionConfig {
            enabled: true,
            algorithm: Algorithm::Aes256Gcm,
            key_provider: KeyProviderConfig::Env {
                variable: "X".into(),
            },
        };
        let status = get_encryption_status(&config);
        assert_eq!(status.algorithm.as_deref(), Some("AES-256-GCM"));

        // ChaCha
        let config = EncryptionConfig {
            enabled: true,
            algorithm: Algorithm::ChaCha20Poly1305,
            key_provider: KeyProviderConfig::Env {
                variable: "X".into(),
            },
        };
        let status = get_encryption_status(&config);
        assert_eq!(status.algorithm.as_deref(), Some("ChaCha20-Poly1305"));
    }

    #[test]
    fn format_status_disabled() {
        let status = EncryptionStatus {
            enabled: false,
            algorithm: None,
            provider_type: None,
            provider_detail: None,
        };
        let formatted = format_encryption_status(&status);

        assert!(formatted.contains("DISABLED"));
        assert!(formatted.contains("Encryption Status"));
    }

    #[test]
    fn format_status_enabled() {
        let status = EncryptionStatus {
            enabled: true,
            algorithm: Some("AES-256-GCM".into()),
            provider_type: Some("file".into()),
            provider_detail: Some("/etc/keys/master.key".into()),
        };
        let formatted = format_encryption_status(&status);

        assert!(formatted.contains("ENABLED"));
        assert!(formatted.contains("AES-256-GCM"));
        assert!(formatted.contains("file"));
        assert!(formatted.contains("/etc/keys/master.key"));
    }

    #[test]
    fn validate_key_file_valid() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("valid.key");

        // Generate a valid key file first
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        assert!(validate_key_file(&key_path).is_ok());
    }

    #[test]
    fn validate_key_file_missing() {
        let result = validate_key_file(&PathBuf::from("/nonexistent/path/missing.key"));

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound, got: {err}"
        );
    }
}
