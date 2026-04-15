//! Encryption-at-rest configuration.
//!
//! [`EncryptionConfig`] lives in the encryption module but is wired into the
//! top-level [`AletheiaDBConfig`](crate::config::AletheiaDBConfig) so that all
//! persistence settings are in one place.

use std::path::PathBuf;

use crate::encryption::factory::Algorithm;

/// Key provider backend configuration.
///
/// Determines where the Master Encryption Key (MEK) is sourced from at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "config-toml",
    serde(tag = "type", rename_all = "snake_case")
)]
/// Configuration options for the Master Encryption Key (MEK) provider.
///
/// # Why?
/// Different environments require different security postures. Development might
/// use a file-based key, while production typically injects keys via environment variables
/// or a KMS (Key Management Service).
pub enum KeyProviderConfig {
    /// Load the MEK from a file on disk (hex or raw binary).
    File {
        /// Path to the key file.
        path: PathBuf,
    },
    /// Load the MEK from an environment variable (hex-encoded).
    Env {
        /// Name of the environment variable.
        variable: String,
    },
}

impl Default for KeyProviderConfig {
    fn default() -> Self {
        Self::Env {
            variable: "ALETHEIADB_MEK".to_string(),
        }
    }
}

/// Top-level encryption-at-rest configuration.
///
/// Disabled by default. When enabled, all persisted data (WAL, indexes, cold
/// storage, checkpoints) is encrypted using per-component DEKs derived from a
/// master encryption key sourced by the configured [`KeyProviderConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct EncryptionConfig {
    /// Whether encryption at rest is enabled.
    pub enabled: bool,
    /// Encryption algorithm selection.
    pub algorithm: Algorithm,
    /// How to obtain the Master Encryption Key.
    pub key_provider: KeyProviderConfig,
}

impl EncryptionConfig {
    /// Return the default disabled configuration.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Create an enabled configuration that reads the MEK from a file.
    #[must_use]
    pub fn file_based(path: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            algorithm: Algorithm::default(),
            key_provider: KeyProviderConfig::File { path: path.into() },
        }
    }

    /// Create an enabled configuration that reads the MEK from an environment variable.
    #[must_use]
    pub fn env_based(var_name: impl Into<String>) -> Self {
        Self {
            enabled: true,
            algorithm: Algorithm::default(),
            key_provider: KeyProviderConfig::Env {
                variable: var_name.into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_disabled() {
        let config = EncryptionConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.algorithm, Algorithm::Auto);
        assert_eq!(
            config.key_provider,
            KeyProviderConfig::Env {
                variable: "ALETHEIADB_MEK".to_string()
            }
        );
    }

    #[test]
    fn disabled_matches_default() {
        assert_eq!(EncryptionConfig::disabled(), EncryptionConfig::default());
    }

    #[test]
    fn file_based_is_enabled() {
        let config = EncryptionConfig::file_based("/tmp/my.key");
        assert!(config.enabled);
        assert_eq!(config.algorithm, Algorithm::Auto);
        assert_eq!(
            config.key_provider,
            KeyProviderConfig::File {
                path: PathBuf::from("/tmp/my.key")
            }
        );
    }

    #[test]
    fn env_based_is_enabled() {
        let config = EncryptionConfig::env_based("MY_CUSTOM_KEY");
        assert!(config.enabled);
        assert_eq!(config.algorithm, Algorithm::Auto);
        assert_eq!(
            config.key_provider,
            KeyProviderConfig::Env {
                variable: "MY_CUSTOM_KEY".to_string()
            }
        );
    }
}
