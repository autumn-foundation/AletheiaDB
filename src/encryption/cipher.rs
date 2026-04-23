//! Cipher abstraction and implementations for authenticated encryption.
//!
//! All ciphers produce output in the wire format: `[nonce][ciphertext][tag]`.

use crate::encryption::EncryptionError;
use rand::RngCore;
use zeroize::Zeroizing;

/// AEAD cipher for encrypting/decrypting data at rest.
///
/// All implementations prepend the nonce and append the auth tag to the output.
pub trait Cipher: Send + Sync {
    /// Encrypt `plaintext`, returning `[nonce || ciphertext || tag]`.
    ///
    /// `aad` is Additional Authenticated Data -- authenticated but not encrypted.
    /// Pass `&[]` if no AAD is needed.
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Decrypt ciphertext produced by [`encrypt`](Cipher::encrypt).
    ///
    /// `aad` must match what was passed to `encrypt`, or decryption will fail.
    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError>;

    /// Byte overhead added by encryption: nonce size + tag size.
    fn overhead(&self) -> usize;

    /// Numeric algorithm identifier for file headers / WAL entries.
    /// - 0 = None/plaintext
    /// - 1 = AES-256-GCM
    /// - 2 = ChaCha20-Poly1305
    fn algorithm_id(&self) -> u8;

    /// Human-readable algorithm name for logging.
    fn algorithm_name(&self) -> &'static str;
}

// ── AES-256-GCM ──────────────────────────────────────────────────

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce as AesNonce};

/// AES-256-GCM authenticated encryption cipher.
///
/// Nonce: 12 bytes (96 bits), Tag: 16 bytes (128 bits).
/// Total overhead: 28 bytes per encrypted payload.
pub struct Aes256GcmCipher {
    inner: Aes256Gcm,
}

/// AES-256-GCM algorithm identifier.
pub const AES_256_GCM_ID: u8 = 1;

/// AES-256-GCM nonce size in bytes.
const AES_NONCE_SIZE: usize = 12;

/// AES-256-GCM tag size in bytes.
const AES_TAG_SIZE: usize = 16;

impl Aes256GcmCipher {
    /// Create a new AES-256-GCM cipher from a 32-byte key.
    ///
    /// # Why?
    /// Automatically leverages hardware acceleration (AES-NI) on x86_64 architectures,
    /// providing line-rate encryption for high-throughput components like the WAL.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::cipher::{Aes256GcmCipher, Cipher};
    /// use zeroize::Zeroizing;
    ///
    /// let key = Zeroizing::new([0u8; 32]);
    /// let cipher = Aes256GcmCipher::new(&key);
    /// assert_eq!(cipher.algorithm_name(), "AES-256-GCM");
    /// ```
    pub fn new(key: &Zeroizing<[u8; 32]>) -> Self {
        let inner = Aes256Gcm::new_from_slice(key.as_ref())
            .expect("32-byte key is always valid for AES-256");
        Self { inner }
    }
}

impl Cipher for Aes256GcmCipher {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        // Generate random 12-byte nonce
        let mut nonce_bytes = [0u8; AES_NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = AesNonce::from_slice(&nonce_bytes);

        // Build AAD payload
        let payload = aes_gcm::aead::Payload {
            msg: plaintext,
            aad,
        };

        // Encrypt (produces ciphertext || tag)
        let ct_with_tag = self
            .inner
            .encrypt(nonce, payload)
            .map_err(|e| EncryptionError::EncryptFailed(e.to_string()))?;

        // Wire format: [nonce:12][ciphertext:N][tag:16]
        let mut output = Vec::with_capacity(AES_NONCE_SIZE + ct_with_tag.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ct_with_tag);
        Ok(output)
    }

    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        let min_len = AES_NONCE_SIZE + AES_TAG_SIZE;
        if ciphertext.len() < min_len {
            return Err(EncryptionError::InvalidCiphertext {
                expected: min_len,
                actual: ciphertext.len(),
            });
        }

        let nonce = AesNonce::from_slice(&ciphertext[..AES_NONCE_SIZE]);
        let ct_and_tag = &ciphertext[AES_NONCE_SIZE..];
        let payload = aes_gcm::aead::Payload {
            msg: ct_and_tag,
            aad,
        };

        self.inner
            .decrypt(nonce, payload)
            .map_err(|e| EncryptionError::DecryptFailed(e.to_string()))
    }

    fn overhead(&self) -> usize {
        AES_NONCE_SIZE + AES_TAG_SIZE // 28
    }

    fn algorithm_id(&self) -> u8 {
        AES_256_GCM_ID
    }

    fn algorithm_name(&self) -> &'static str {
        "AES-256-GCM"
    }
}

// ── ChaCha20-Poly1305 ────────────────────────────────────────────

use chacha20poly1305::ChaCha20Poly1305 as ChaChaInner;
use chacha20poly1305::Nonce as ChaChaNonce;

/// ChaCha20-Poly1305 authenticated encryption cipher.
///
/// Nonce: 12 bytes, Tag: 16 bytes. Total overhead: 28 bytes.
/// Preferred on platforms without AES-NI hardware acceleration.
pub struct ChaCha20Poly1305Cipher {
    inner: ChaChaInner,
}

/// ChaCha20-Poly1305 algorithm identifier.
pub const CHACHA20_POLY1305_ID: u8 = 2;

/// ChaCha20 nonce size in bytes.
const CHACHA_NONCE_SIZE: usize = 12;

/// Poly1305 tag size in bytes.
const CHACHA_TAG_SIZE: usize = 16;

impl ChaCha20Poly1305Cipher {
    /// Create a new ChaCha20-Poly1305 cipher from a 32-byte key.
    ///
    /// # Why?
    /// Serves as the high-performance fallback for ARM/Edge deployments where AES-NI
    /// hardware acceleration might not be available, ensuring predictable performance.
    ///
    /// ## Examples
    /// ```
    /// use aletheiadb::encryption::cipher::{ChaCha20Poly1305Cipher, Cipher};
    /// use zeroize::Zeroizing;
    ///
    /// let key = Zeroizing::new([0u8; 32]);
    /// let cipher = ChaCha20Poly1305Cipher::new(&key);
    /// assert_eq!(cipher.algorithm_name(), "ChaCha20-Poly1305");
    /// ```
    pub fn new(key: &Zeroizing<[u8; 32]>) -> Self {
        let inner = ChaChaInner::new_from_slice(key.as_ref())
            .expect("32-byte key is always valid for ChaCha20");
        Self { inner }
    }
}

impl Cipher for ChaCha20Poly1305Cipher {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        let mut nonce_bytes = [0u8; CHACHA_NONCE_SIZE];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = ChaChaNonce::from_slice(&nonce_bytes);

        let payload = chacha20poly1305::aead::Payload {
            msg: plaintext,
            aad,
        };

        let ct_with_tag = self
            .inner
            .encrypt(nonce, payload)
            .map_err(|e| EncryptionError::EncryptFailed(e.to_string()))?;

        let mut output = Vec::with_capacity(CHACHA_NONCE_SIZE + ct_with_tag.len());
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ct_with_tag);
        Ok(output)
    }

    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        let min_len = CHACHA_NONCE_SIZE + CHACHA_TAG_SIZE;
        if ciphertext.len() < min_len {
            return Err(EncryptionError::InvalidCiphertext {
                expected: min_len,
                actual: ciphertext.len(),
            });
        }

        let nonce = ChaChaNonce::from_slice(&ciphertext[..CHACHA_NONCE_SIZE]);
        let ct_and_tag = &ciphertext[CHACHA_NONCE_SIZE..];
        let payload = chacha20poly1305::aead::Payload {
            msg: ct_and_tag,
            aad,
        };

        self.inner
            .decrypt(nonce, payload)
            .map_err(|e| EncryptionError::DecryptFailed(e.to_string()))
    }

    fn overhead(&self) -> usize {
        CHACHA_NONCE_SIZE + CHACHA_TAG_SIZE // 28
    }

    fn algorithm_id(&self) -> u8 {
        CHACHA20_POLY1305_ID
    }

    fn algorithm_name(&self) -> &'static str {
        "ChaCha20-Poly1305"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::rng().fill_bytes(key.as_mut());
        key
    }

    // ── AES-256-GCM ──

    #[test]
    fn aes256gcm_roundtrip() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let plaintext = b"Hello, AletheiaDB!";
        let aad = b"test-context";

        let encrypted = cipher.encrypt(plaintext, aad).unwrap();
        let decrypted = cipher.decrypt(&encrypted, aad).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes256gcm_overhead() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        assert_eq!(cipher.overhead(), 28);
    }

    #[test]
    fn aes256gcm_output_size() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let plaintext = b"short";

        let encrypted = cipher.encrypt(plaintext, &[]).unwrap();
        assert_eq!(encrypted.len(), plaintext.len() + cipher.overhead());
    }

    #[test]
    fn aes256gcm_different_nonces() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let plaintext = b"same data twice";

        let enc1 = cipher.encrypt(plaintext, &[]).unwrap();
        let enc2 = cipher.encrypt(plaintext, &[]).unwrap();

        // Different nonces -> different ciphertext
        assert_ne!(enc1, enc2);
        // But both decrypt to same plaintext
        assert_eq!(cipher.decrypt(&enc1, &[]).unwrap(), plaintext);
        assert_eq!(cipher.decrypt(&enc2, &[]).unwrap(), plaintext);
    }

    #[test]
    fn aes256gcm_wrong_aad_fails() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let encrypted = cipher.encrypt(b"secret", b"correct-aad").unwrap();

        let result = cipher.decrypt(&encrypted, b"wrong-aad");
        assert!(result.is_err());
    }

    #[test]
    fn aes256gcm_wrong_key_fails() {
        let key1 = test_key();
        let key2 = test_key();
        let c1 = Aes256GcmCipher::new(&key1);
        let c2 = Aes256GcmCipher::new(&key2);

        let encrypted = c1.encrypt(b"data", &[]).unwrap();
        let result = c2.decrypt(&encrypted, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn aes256gcm_tampered_ciphertext_fails() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let mut encrypted = cipher.encrypt(b"important", &[]).unwrap();

        // Flip a byte in the ciphertext portion
        let mid = AES_NONCE_SIZE + 2;
        encrypted[mid] ^= 0xFF;

        let result = cipher.decrypt(&encrypted, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn aes256gcm_too_short_ciphertext() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);

        let result = cipher.decrypt(&[0u8; 10], &[]);
        assert!(matches!(
            result,
            Err(EncryptionError::InvalidCiphertext {
                expected: 28,
                actual: 10
            })
        ));
    }

    #[test]
    fn aes256gcm_empty_plaintext() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);

        let encrypted = cipher.encrypt(&[], &[]).unwrap();
        assert_eq!(encrypted.len(), cipher.overhead());

        let decrypted = cipher.decrypt(&encrypted, &[]).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn aes256gcm_large_payload() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        let plaintext = vec![0xABu8; 1_000_000]; // 1MB

        let encrypted = cipher.encrypt(&plaintext, &[]).unwrap();
        let decrypted = cipher.decrypt(&encrypted, &[]).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes256gcm_algorithm_id() {
        let key = test_key();
        let cipher = Aes256GcmCipher::new(&key);
        assert_eq!(cipher.algorithm_id(), 1);
        assert_eq!(cipher.algorithm_name(), "AES-256-GCM");
    }

    // ── ChaCha20-Poly1305 ──

    #[test]
    fn chacha20_roundtrip() {
        let key = test_key();
        let cipher = ChaCha20Poly1305Cipher::new(&key);
        let plaintext = b"Hello, ChaCha!";

        let encrypted = cipher.encrypt(plaintext, &[]).unwrap();
        let decrypted = cipher.decrypt(&encrypted, &[]).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn chacha20_with_aad() {
        let key = test_key();
        let cipher = ChaCha20Poly1305Cipher::new(&key);
        let plaintext = b"authenticated data";
        let aad = b"metadata";

        let encrypted = cipher.encrypt(plaintext, aad).unwrap();
        let decrypted = cipher.decrypt(&encrypted, aad).unwrap();
        assert_eq!(decrypted, plaintext);

        // Wrong AAD fails
        assert!(cipher.decrypt(&encrypted, b"wrong").is_err());
    }

    #[test]
    fn chacha20_algorithm_id() {
        let key = test_key();
        let cipher = ChaCha20Poly1305Cipher::new(&key);
        assert_eq!(cipher.algorithm_id(), 2);
        assert_eq!(cipher.algorithm_name(), "ChaCha20-Poly1305");
    }

    #[test]
    fn chacha20_wrong_key_fails() {
        let k1 = test_key();
        let k2 = test_key();
        let c1 = ChaCha20Poly1305Cipher::new(&k1);
        let c2 = ChaCha20Poly1305Cipher::new(&k2);

        let encrypted = c1.encrypt(b"secret", &[]).unwrap();
        assert!(c2.decrypt(&encrypted, &[]).is_err());
    }

    // ── Cross-cipher interop ──

    #[test]
    fn different_ciphers_not_interoperable() {
        let key = test_key();
        let aes = Aes256GcmCipher::new(&key);
        let chacha = ChaCha20Poly1305Cipher::new(&key);

        let encrypted = aes.encrypt(b"data", &[]).unwrap();
        // ChaCha cannot decrypt AES-GCM output
        assert!(chacha.decrypt(&encrypted, &[]).is_err());
    }
}
