//! Central encryption manager that wires together config, key provider, key
//! derivation, and cipher creation at database startup.
//!
//! [`EncryptionManager`] is the single entry-point that the rest of the database
//! uses to obtain per-component ciphers.

use std::sync::Arc;

use crate::encryption::audit::{AuditEvent, EncryptionAuditLogger};
use crate::encryption::cipher::Cipher;
use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
use crate::encryption::error::KeyProviderError;
use crate::encryption::factory::create_cipher;
use crate::encryption::key_derivation::{
    CHECKPOINT_DEK_CONTEXT, COLD_DEK_CONTEXT, INDEX_DEK_CONTEXT, KeyDerivation, WAL_DEK_CONTEXT,
};
use crate::encryption::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider};

/// The key version tracked by the manager for the currently-loaded MEK.
///
/// Versioning is owned by the (not-yet-built) rotation engine (#488); until it
/// lands the manager always reports version 1 for the freshly-loaded key.
const CURRENT_KEY_VERSION: u32 = 1;

/// Reduce a [`KeyProviderError`] to a stable, key-safe category token.
///
/// # Why?
/// The audit log must **never** leak secrets. A raw error `Display` could embed
/// a key file path (or, in principle, other sensitive context), so the audit
/// path records only this opaque category -- never the full error string.
fn error_category(err: &KeyProviderError) -> &'static str {
    match err {
        KeyProviderError::KeyNotFound => "key_not_found",
        KeyProviderError::AccessDenied(_) => "access_denied",
        KeyProviderError::Unavailable(_) => "provider_unavailable",
        KeyProviderError::InvalidKeyFormat(_) => "invalid_key_format",
        KeyProviderError::RotationNotSupported => "rotation_not_supported",
        KeyProviderError::Io(_) => "io_error",
        KeyProviderError::Provider(_) => "provider_error",
    }
}

/// Central manager that owns per-component ciphers for encryption at rest.
///
/// Created once at database startup via [`from_config`](Self::from_config) and
/// then shared (via `Arc`) with every persistence subsystem that needs to
/// encrypt/decrypt data.
pub struct EncryptionManager {
    wal_cipher: Arc<dyn Cipher>,
    index_cipher: Arc<dyn Cipher>,
    cold_cipher: Arc<dyn Cipher>,
    checkpoint_cipher: Arc<dyn Cipher>,
    provider_name: String,
    /// Structured encryption audit logger (Issue #489). Emits `key.loaded` at
    /// construction; available to the rotation engine (#488) for
    /// `key.rotation.*` events via [`audit_logger`](Self::audit_logger).
    audit: EncryptionAuditLogger,
}

impl std::fmt::Debug for EncryptionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionManager")
            .field("wal_cipher", &self.wal_cipher.algorithm_name())
            .field("index_cipher", &self.index_cipher.algorithm_name())
            .field("cold_cipher", &self.cold_cipher.algorithm_name())
            .field(
                "checkpoint_cipher",
                &self.checkpoint_cipher.algorithm_name(),
            )
            .field("provider_name", &self.provider_name)
            .finish()
    }
}

impl EncryptionManager {
    /// Build an [`EncryptionManager`] from an [`EncryptionConfig`].
    ///
    /// This:
    /// 1. Creates the appropriate [`KeyProvider`] based on config.
    /// 2. Loads the Master Encryption Key (MEK).
    /// 3. Derives four per-component DEKs via HKDF-SHA256.
    /// 4. Creates four independent ciphers (one per storage component).
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError`] if the MEK cannot be loaded (missing file,
    /// missing env var, invalid format, etc.).
    pub fn from_config(config: &EncryptionConfig) -> Result<Self, KeyProviderError> {
        let audit = EncryptionAuditLogger::from_audit_config(&config.audit);
        Self::from_config_with_logger(config, audit)
    }

    /// Build an [`EncryptionManager`] from config with an explicitly-provided
    /// audit logger.
    ///
    /// [`from_config`](Self::from_config) delegates here after constructing a
    /// logger from `config.audit`. This variant lets callers (and tests) inject
    /// a preconfigured logger.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError`] if the MEK cannot be loaded. On failure a
    /// `key.access.denied` audit event (carrying only a key-safe error
    /// category) is emitted before returning.
    pub fn from_config_with_logger(
        config: &EncryptionConfig,
        audit: EncryptionAuditLogger,
    ) -> Result<Self, KeyProviderError> {
        // 1. Create the key provider.
        let provider: Box<dyn KeyProvider> = match &config.key_provider {
            KeyProviderConfig::File { path } => Box::new(FileKeyProvider::new(path)),
            KeyProviderConfig::Env { variable } => Box::new(EnvKeyProvider::new(variable)),
        };

        let name = provider.provider_name().to_string();

        // 2. Load the MEK, auditing success (`key.loaded`) and failure
        //    (`key.access.denied` with a key-safe category, never a path or key
        //    material).
        let mek = match provider.get_mek() {
            Ok(mek) => {
                audit.log(&AuditEvent::KeyLoaded {
                    provider: name.clone(),
                    key_version: CURRENT_KEY_VERSION,
                });
                mek
            }
            Err(e) => {
                audit.log(&AuditEvent::KeyAccessDenied {
                    provider: name.clone(),
                    error: error_category(&e).to_string(),
                });
                return Err(e);
            }
        };

        // NOTE: rotation audit events (`key.rotation.started` /
        // `key.rotation.completed` / `key.rotation.failed`) are emitted by the
        // key-rotation engine, which does not exist yet -- wired by #488 via
        // `EncryptionManager::audit_logger()`.

        // 3. Derive per-component DEKs.
        let kd = KeyDerivation::new(mek);
        let wal_dek = kd
            .derive_dek(WAL_DEK_CONTEXT)
            .map_err(|e| KeyProviderError::Provider(Box::new(e)))?;
        let index_dek = kd
            .derive_dek(INDEX_DEK_CONTEXT)
            .map_err(|e| KeyProviderError::Provider(Box::new(e)))?;
        let cold_dek = kd
            .derive_dek(COLD_DEK_CONTEXT)
            .map_err(|e| KeyProviderError::Provider(Box::new(e)))?;
        let checkpoint_dek = kd
            .derive_dek(CHECKPOINT_DEK_CONTEXT)
            .map_err(|e| KeyProviderError::Provider(Box::new(e)))?;

        // 4. Create ciphers.
        let algorithm = config.algorithm;
        Ok(Self {
            wal_cipher: Arc::from(create_cipher(algorithm, &wal_dek)),
            index_cipher: Arc::from(create_cipher(algorithm, &index_dek)),
            cold_cipher: Arc::from(create_cipher(algorithm, &cold_dek)),
            checkpoint_cipher: Arc::from(create_cipher(algorithm, &checkpoint_dek)),
            provider_name: name,
            audit,
        })
    }

    /// The encryption audit logger owned by this manager.
    ///
    /// # Why?
    /// The key-rotation engine (#488) will use this to emit `key.rotation.*`
    /// audit events, keeping all encryption audit emission on one logger with a
    /// single configured destination and instance id.
    #[must_use]
    pub fn audit_logger(&self) -> &EncryptionAuditLogger {
        &self.audit
    }

    /// Cipher for WAL encryption/decryption.
    ///
    /// # Why?
    /// Each component gets its own derived key (DEK) to limit the blast radius if a single
    /// component's key is compromised. The WAL cipher is specifically for the write-ahead log.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::manager::EncryptionManager;
    /// use aletheiadb::encryption::config::EncryptionConfig;
    /// use aletheiadb::encryption::key_provider::FileKeyProvider;
    /// use aletheiadb::encryption::cipher::Cipher;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let key_path = dir.path().join("test.key");
    /// FileKeyProvider::generate_key_file(&key_path).unwrap();
    ///
    /// let config = EncryptionConfig::file_based(&key_path);
    /// let manager = EncryptionManager::from_config(&config).unwrap();
    ///
    /// let encrypted = manager.wal_cipher().encrypt(b"data", b"aad").unwrap();
    /// ```
    #[must_use]
    pub fn wal_cipher(&self) -> &Arc<dyn Cipher> {
        &self.wal_cipher
    }

    /// Cipher for index persistence encryption/decryption.
    ///
    /// # Why?
    /// The index cipher uses a separate derived key from the WAL and cold storage to ensure
    /// cryptographic isolation between the vector search indexes and core database data.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::manager::EncryptionManager;
    /// use aletheiadb::encryption::config::EncryptionConfig;
    /// use aletheiadb::encryption::key_provider::FileKeyProvider;
    /// use aletheiadb::encryption::cipher::Cipher;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let key_path = dir.path().join("test.key");
    /// FileKeyProvider::generate_key_file(&key_path).unwrap();
    ///
    /// let config = EncryptionConfig::file_based(&key_path);
    /// let manager = EncryptionManager::from_config(&config).unwrap();
    ///
    /// let encrypted = manager.index_cipher().encrypt(b"data", b"aad").unwrap();
    /// ```
    #[must_use]
    pub fn index_cipher(&self) -> &Arc<dyn Cipher> {
        &self.index_cipher
    }

    /// Cipher for cold storage encryption/decryption.
    ///
    /// # Why?
    /// Cold storage files might be archived to untrusted cloud storage (like S3). Using a
    /// dedicated key ensures that even if an attacker obtains an archive, they cannot decrypt
    /// it without this specific DEK.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::manager::EncryptionManager;
    /// use aletheiadb::encryption::config::EncryptionConfig;
    /// use aletheiadb::encryption::key_provider::FileKeyProvider;
    /// use aletheiadb::encryption::cipher::Cipher;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let key_path = dir.path().join("test.key");
    /// FileKeyProvider::generate_key_file(&key_path).unwrap();
    ///
    /// let config = EncryptionConfig::file_based(&key_path);
    /// let manager = EncryptionManager::from_config(&config).unwrap();
    ///
    /// let encrypted = manager.cold_cipher().encrypt(b"data", b"aad").unwrap();
    /// ```
    #[must_use]
    pub fn cold_cipher(&self) -> &Arc<dyn Cipher> {
        &self.cold_cipher
    }

    /// Cipher for checkpoint encryption/decryption.
    ///
    /// # Why?
    /// Checkpoint files contain complete database snapshots. Isolating their encryption
    /// from active WAL segments prevents attackers from exploiting potential nonce-reuse
    /// vulnerabilities across different snapshot versions.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::manager::EncryptionManager;
    /// use aletheiadb::encryption::config::EncryptionConfig;
    /// use aletheiadb::encryption::key_provider::FileKeyProvider;
    /// use aletheiadb::encryption::cipher::Cipher;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let key_path = dir.path().join("test.key");
    /// FileKeyProvider::generate_key_file(&key_path).unwrap();
    ///
    /// let config = EncryptionConfig::file_based(&key_path);
    /// let manager = EncryptionManager::from_config(&config).unwrap();
    ///
    /// let encrypted = manager.checkpoint_cipher().encrypt(b"data", b"aad").unwrap();
    /// ```
    #[must_use]
    pub fn checkpoint_cipher(&self) -> &Arc<dyn Cipher> {
        &self.checkpoint_cipher
    }

    /// Human-readable name of the key provider backend (e.g., `"file"`, `"env"`).
    ///
    /// # Why?
    /// This is used exclusively for diagnostic logging and telemetry, allowing operators
    /// to see where the master key was sourced without exposing the key itself.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::manager::EncryptionManager;
    /// use aletheiadb::encryption::config::EncryptionConfig;
    /// use aletheiadb::encryption::key_provider::FileKeyProvider;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let key_path = dir.path().join("test.key");
    /// FileKeyProvider::generate_key_file(&key_path).unwrap();
    ///
    /// let config = EncryptionConfig::file_based(&key_path);
    /// let manager = EncryptionManager::from_config(&config).unwrap();
    ///
    /// assert_eq!(manager.provider_name(), "file");
    /// ```
    #[must_use]
    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::key_provider::FileKeyProvider;

    #[test]
    fn from_config_with_file_provider() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let config = EncryptionConfig::file_based(&key_path);
        let mgr = EncryptionManager::from_config(&config).unwrap();

        assert_eq!(mgr.provider_name(), "file");

        // All four ciphers should round-trip independently.
        let plaintext = b"manager test payload";
        let aad = b"test-aad";

        for (label, cipher) in [
            ("wal", mgr.wal_cipher()),
            ("index", mgr.index_cipher()),
            ("cold", mgr.cold_cipher()),
            ("checkpoint", mgr.checkpoint_cipher()),
        ] {
            let encrypted = cipher.encrypt(plaintext, aad).unwrap();
            let decrypted = cipher.decrypt(&encrypted, aad).unwrap();
            assert_eq!(
                decrypted.as_slice(),
                plaintext,
                "{label} cipher roundtrip failed"
            );
        }
    }

    #[test]
    fn from_config_different_ciphers_per_component() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let config = EncryptionConfig::file_based(&key_path);
        let mgr = EncryptionManager::from_config(&config).unwrap();

        let plaintext = b"cross-cipher test";
        let aad = b"test-aad";

        // Encrypt with WAL cipher, attempt decrypt with index cipher -- must fail.
        let wal_encrypted = mgr.wal_cipher().encrypt(plaintext, aad).unwrap();
        let result = mgr.index_cipher().decrypt(&wal_encrypted, aad);
        assert!(
            result.is_err(),
            "Decrypting WAL ciphertext with index cipher should fail (different DEKs)"
        );
    }

    #[test]
    fn from_config_missing_key_file() {
        let config = EncryptionConfig::file_based("/nonexistent/path/missing.key");
        let err = EncryptionManager::from_config(&config).unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound, got: {err}"
        );
    }

    #[test]
    fn from_config_emits_key_loaded_audit_event() {
        use crate::encryption::audit::AuditLevel;

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let config = EncryptionConfig::file_based(&key_path);
        let (logger, buf) = EncryptionAuditLogger::with_capture(AuditLevel::KeyEvents, "node-1");

        let mgr = EncryptionManager::from_config_with_logger(&config, logger).unwrap();
        assert_eq!(mgr.provider_name(), "file");

        let lines = buf.lock().unwrap();
        assert_eq!(lines.len(), 1, "exactly one key.loaded event");
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["event"], "key.loaded");
        assert_eq!(v["data"]["provider_type"], "file");
        assert_eq!(v["data"]["key_version"], 1);
    }

    #[test]
    fn from_config_emits_key_access_denied_on_failure() {
        use crate::encryption::audit::AuditLevel;

        // A file provider pointed at a missing key => load failure.
        let config = EncryptionConfig::file_based("/nonexistent/path/missing.key");
        let (logger, buf) = EncryptionAuditLogger::with_capture(AuditLevel::KeyEvents, "node-1");

        let err = EncryptionManager::from_config_with_logger(&config, logger).unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyNotFound));

        let lines = buf.lock().unwrap();
        assert_eq!(lines.len(), 1, "exactly one key.access.denied event");
        let line = &lines[0];
        // No path leakage: the raw key file path must not appear in the audit line.
        assert!(
            !line.contains("missing.key") && !line.contains("/nonexistent"),
            "audit line leaked the key file path: {line}"
        );
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["event"], "key.access.denied");
        assert_eq!(v["level"], "error");
        assert_eq!(v["data"]["provider"], "file");
        assert_eq!(v["data"]["error"], "key_not_found");
    }

    #[test]
    fn error_category_never_includes_details() {
        // Categories are fixed tokens -- they can never carry a path or key bytes.
        assert_eq!(
            error_category(&KeyProviderError::KeyNotFound),
            "key_not_found"
        );
        assert_eq!(
            error_category(&KeyProviderError::InvalidKeyFormat(
                "/secret/path leaked".into()
            )),
            "invalid_key_format"
        );
        assert_eq!(
            error_category(&KeyProviderError::AccessDenied("secret".into())),
            "access_denied"
        );
    }

    #[test]
    fn from_config_default_audit_disabled_is_silent() {
        // Default config (no audit block) => disabled logger, zero output.
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");
        FileKeyProvider::generate_key_file(&key_path).unwrap();

        let config = EncryptionConfig::file_based(&key_path);
        assert!(!config.audit.enabled);
        // Must construct successfully with auditing disabled (no panic, no output).
        let mgr = EncryptionManager::from_config(&config).unwrap();
        assert_eq!(
            mgr.audit_logger().level(),
            crate::encryption::audit::AuditLevel::None
        );
    }
}
