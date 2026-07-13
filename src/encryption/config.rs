//! Encryption-at-rest configuration.
//!
//! [`EncryptionConfig`] lives in the encryption module but is wired into the
//! top-level [`AletheiaDBConfig`](crate::config::AletheiaDBConfig) so that all
//! persistence settings are in one place.

use std::path::PathBuf;

use crate::encryption::audit::AuditLevel;
use crate::encryption::factory::Algorithm;

/// Where audit log lines are written.
///
/// `syslog` is accepted for forward compatibility but is **not yet
/// implemented** -- when selected, the logger degrades to stderr with a
/// warning (see Issue #489 follow-ups).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum AuditDestination {
    /// Write structured JSON lines to standard output.
    #[default]
    Stdout,
    /// Append structured JSON lines to the file at
    /// [`AuditConfig::file_path`].
    File,
    /// Syslog (not yet implemented; falls back to stderr).
    Syslog,
}

/// Encryption audit-logging configuration (`[encryption.audit]`, Issue #489).
///
/// Disabled by default: an [`EncryptionConfig`] without an `[encryption.audit]`
/// block produces **zero** audit output, preserving prior behavior exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct AuditConfig {
    /// Whether audit logging is enabled. When `false`, no events are emitted.
    pub enabled: bool,
    /// Which events to emit. Defaults to `key_events` (only meaningful when
    /// `enabled`).
    pub level: AuditLevel,
    /// Where to write audit lines.
    pub destination: AuditDestination,
    /// Target file when `destination = "file"`. If unset with a `file`
    /// destination, the logger falls back to stdout with a warning.
    pub file_path: Option<PathBuf>,
    /// Optional stable instance identifier stamped into every line. When unset,
    /// a per-process default is generated.
    pub instance_id: Option<String>,
    /// Log-file rotation policy (e.g. `"daily"`). **Accepted but not yet
    /// enforced** -- log-file rotation is a documented Issue #489 follow-up.
    pub rotation: Option<String>,
    /// Log retention window in days. **Accepted but not yet enforced** --
    /// retention pruning is a documented Issue #489 follow-up.
    pub retention_days: Option<u32>,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            level: AuditLevel::KeyEvents,
            destination: AuditDestination::Stdout,
            file_path: None,
            instance_id: None,
            rotation: None,
            retention_days: None,
        }
    }
}

/// Key provider backend configuration.
///
/// Determines where the Master Encryption Key (MEK) is sourced from at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", rename_all = "snake_case"))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EncryptionConfig {
    /// Whether encryption at rest is enabled.
    pub enabled: bool,
    /// Encryption algorithm selection.
    pub algorithm: Algorithm,
    /// How to obtain the Master Encryption Key.
    pub key_provider: KeyProviderConfig,
    /// Audit-logging configuration (Issue #489). Disabled by default.
    pub audit: AuditConfig,
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
            audit: AuditConfig::default(),
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
            audit: AuditConfig::default(),
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

    #[test]
    fn audit_disabled_by_default() {
        let config = EncryptionConfig::default();
        assert!(!config.audit.enabled);
        assert_eq!(config.audit.level, AuditLevel::KeyEvents);
        assert_eq!(config.audit.destination, AuditDestination::Stdout);
        assert!(config.audit.file_path.is_none());
        assert!(config.audit.rotation.is_none());
        assert!(config.audit.retention_days.is_none());
    }

    #[cfg(feature = "config-toml")]
    #[test]
    fn audit_config_round_trips_via_toml() {
        let toml_str = r#"
enabled = true

[audit]
enabled = true
level = "all_operations"
destination = "file"
file_path = "logs/encryption-audit.log"
rotation = "daily"
retention_days = 90
"#;

        let parsed: EncryptionConfig = toml::from_str(toml_str).expect("parse encryption config");
        assert!(parsed.enabled);
        assert!(parsed.audit.enabled);
        assert_eq!(parsed.audit.level, AuditLevel::AllOperations);
        assert_eq!(parsed.audit.destination, AuditDestination::File);
        assert_eq!(
            parsed.audit.file_path,
            Some(PathBuf::from("logs/encryption-audit.log"))
        );
        assert_eq!(parsed.audit.rotation.as_deref(), Some("daily"));
        assert_eq!(parsed.audit.retention_days, Some(90));

        // Round-trip: re-serialize and re-parse yields an equal config.
        let serialized = toml::to_string(&parsed).expect("serialize");
        let reparsed: EncryptionConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(parsed, reparsed);
    }

    #[cfg(feature = "config-toml")]
    #[test]
    fn audit_level_serde_snake_case() {
        assert_eq!(
            toml::from_str::<EncryptionConfig>("[audit]\nlevel = \"key_events\"\n")
                .unwrap()
                .audit
                .level,
            AuditLevel::KeyEvents
        );
        assert_eq!(
            toml::from_str::<EncryptionConfig>("[audit]\nlevel = \"none\"\n")
                .unwrap()
                .audit
                .level,
            AuditLevel::None
        );
    }

    #[cfg(feature = "config-toml")]
    #[test]
    fn encryption_config_without_audit_block_defaults_disabled() {
        // A config with no [audit] block must leave auditing disabled -- zero
        // behavior change for existing configs.
        let parsed: EncryptionConfig =
            toml::from_str("enabled = false\n").expect("parse without audit block");
        assert!(!parsed.audit.enabled);
    }
}
