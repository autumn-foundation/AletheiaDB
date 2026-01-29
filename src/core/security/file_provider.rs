#![cfg(feature = "tokio")]

use super::{KeyError, KeyProvider, MasterKey};
use argon2::{Algorithm, Argon2, Params, Version};
use async_trait::async_trait;
use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit},
};
use std::path::PathBuf;
use tokio::fs;
use zeroize::Zeroizing;

const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
// Key is 32 bytes
const HEADER_SIZE: usize = SALT_SIZE + NONCE_SIZE;
const MIN_FILE_SIZE: usize = HEADER_SIZE + TAG_SIZE;

/// Provider that loads the master key from an encrypted file.
///
/// The file format is:
/// [SALT (16 bytes)] [NONCE (12 bytes)] [CIPHERTEXT (encrypted MasterKey)]
///
/// The key is encrypted using ChaCha20Poly1305.
/// The encryption key is derived from the passphrase using Argon2id.
pub struct FileKeyProvider {
    path: PathBuf,
    passphrase: Zeroizing<String>,
}

impl FileKeyProvider {
    /// Create a new FileKeyProvider.
    pub fn new(path: impl Into<PathBuf>, passphrase: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            passphrase: Zeroizing::new(passphrase.into()),
        }
    }
}

#[async_trait]
impl KeyProvider for FileKeyProvider {
    async fn get_master_key(&self) -> Result<MasterKey, KeyError> {
        // 1. Read file
        let data = fs::read(&self.path).await.map_err(|e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyError::NotFound
            } else {
                KeyError::ConfigError(format!("Failed to read key file: {}", e))
            }
        })?;

        // 2. Validate length
        if data.len() < MIN_FILE_SIZE {
            return Err(KeyError::ConfigError(
                "Key file corrupted (too short)".to_string(),
            ));
        }

        let salt = &data[0..SALT_SIZE];
        let nonce_bytes = &data[SALT_SIZE..HEADER_SIZE];
        let ciphertext = &data[HEADER_SIZE..];

        // 3. Derive KEK
        // Use explicitly configured params: 64MB memory, 3 iterations, 4 threads
        // Panic on invalid params as these are hardcoded constants.
        let params = Params::new(64 * 1024, 3, 4, Some(32))
            .expect("Hardcoded Argon2 parameters should be valid");
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let mut kek = Zeroizing::new([0u8; 32]);
        argon2
            .hash_password_into(self.passphrase.as_bytes(), salt, &mut *kek)
            .map_err(|e| KeyError::ConfigError(format!("Argon2 KDF failed: {}", e)))?;

        // 4. Decrypt
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&*kek));
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = Zeroizing::new(
            cipher
                .decrypt(nonce, ciphertext)
                .map_err(|_| KeyError::DecryptionFailed)?,
        );

        if plaintext.len() != 32 {
            return Err(KeyError::ConfigError(
                "Decrypted key has invalid length".to_string(),
            ));
        }

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&*plaintext);

        Ok(MasterKey::new(key_bytes, 1))
    }

    async fn get_key_version(&self, version: u32) -> Result<MasterKey, KeyError> {
        // Simple file provider currently only supports the "current" version on disk.
        // If versions match, return it.
        let key = self.get_master_key().await?;
        if key.version() == version {
            Ok(key)
        } else {
            Err(KeyError::NotFound)
        }
    }

    fn current_version(&self) -> u32 {
        1
    }

    fn name(&self) -> &str {
        "FileKeyProvider"
    }
}
