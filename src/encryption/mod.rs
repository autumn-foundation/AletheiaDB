//! Encryption at rest for AletheiaDB.
//!
//! Provides pluggable key management and cipher abstractions for encrypting
//! all persisted data (WAL, indexes, cold storage). See ADR-0028.
//!
//! # Architecture
//!
//! ```text
//! KeyProvider -> MEK -> HKDF -> DEKs -> Cipher -> Encrypted Data
//! ```
//!
//! - **KeyProvider**: Sources the Master Encryption Key (file, env, KMS)
//! - **KeyDerivation**: Derives per-component DEKs via HKDF-SHA256
//! - **Cipher**: AES-256-GCM or ChaCha20-Poly1305 AEAD encryption

pub mod audit;
pub mod cipher;
pub mod cli;
pub mod config;
pub mod error;
pub mod factory;
pub mod key_derivation;
pub mod key_provider;
#[cfg(feature = "encryption-aws-kms")]
pub mod kms_provider;
pub mod manager;
pub mod passphrase;
pub mod rotation;
#[cfg(feature = "encryption-vault")]
pub mod vault_provider;
pub mod wal_encryption;

pub use audit::{AuditEvent, AuditLevel, EncryptionAuditLogger};
pub use cipher::{
    AES_256_GCM_ID, Aes256GcmCipher, CHACHA20_POLY1305_ID, ChaCha20Poly1305Cipher, Cipher,
};
pub use cli::{
    EncryptionStatus, KeyGenResult, format_encryption_status, generate_key,
    generate_key_with_overwrite, generate_passphrase_key_with_overwrite, get_encryption_status,
    validate_key_file,
};
pub use config::{AuditConfig, AuditDestination, EncryptionConfig, KeyProviderConfig};
pub use error::{EncryptionError, KeyDerivationError, KeyProviderError};
pub use factory::{Algorithm, algorithm_from_id, create_cipher};
pub use key_derivation::{
    CHECKPOINT_DEK_CONTEXT, COLD_DEK_CONTEXT, INDEX_DEK_CONTEXT, KeyDerivation, WAL_DEK_CONTEXT,
};
pub use key_provider::{EnvKeyProvider, FileKeyProvider, KeyFormat, KeyProvider};
#[cfg(feature = "encryption-aws-kms")]
pub use kms_provider::{KmsConfig, KmsKeyProvider};
pub use manager::EncryptionManager;
pub use passphrase::{
    PassphraseFileKeyProvider, PassphraseKdf, generate_passphrase_key_file,
    generate_passphrase_key_file_with_kdf,
};
pub use rotation::{KeyRotationManager, KeyVersion, RotationState};
#[cfg(feature = "encryption-vault")]
pub use vault_provider::{VaultConfig, VaultKeyProvider};
pub use wal_encryption::{decrypt_wal_payload, encrypt_wal_payload};
