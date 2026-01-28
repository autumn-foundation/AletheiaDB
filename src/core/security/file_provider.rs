use super::{KeyProvider, MasterKey, KeyError};
use async_trait::async_trait;
use std::path::PathBuf;
use zeroize::Zeroizing;
use std::fs;
use argon2::Argon2;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce
};

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
        let data = fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyError::NotFound
            } else {
                KeyError::ConfigError(format!("Failed to read key file: {}", e))
            }
        })?;

        // 2. Validate length (Salt 16 + Nonce 12 + Tag 16 + Data 32 = 76 bytes)
        // ChaCha20Poly1305 tag is 16 bytes.
        if data.len() < 16 + 12 + 16 {
             return Err(KeyError::ConfigError("Key file corrupted (too short)".to_string()));
        }

        let salt = &data[0..16];
        let nonce_bytes = &data[16..28];
        let ciphertext = &data[28..];

        // 3. Derive KEK
        let mut kek = [0u8; 32];
        let argon2 = Argon2::default();
        if argon2.hash_password_into(
            self.passphrase.as_bytes(),
            salt,
            &mut kek
        ).is_err() {
            return Err(KeyError::DecryptionFailed);
        }

        // 4. Decrypt
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| KeyError::DecryptionFailed)?;

        if plaintext.len() != 32 {
            return Err(KeyError::ConfigError("Decrypted key has invalid length".to_string()));
        }

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&plaintext);

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
