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

pub mod cipher;
pub mod error;

pub use cipher::{
    AES_256_GCM_ID, Aes256GcmCipher, CHACHA20_POLY1305_ID, ChaCha20Poly1305Cipher, Cipher,
};
pub use error::{EncryptionError, KeyDerivationError, KeyProviderError};
