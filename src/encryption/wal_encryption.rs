//! WAL entry payload encryption/decryption.
//!
//! Pure functions that encrypt/decrypt the **payload portion** of a serialized
//! WAL entry while leaving the header and op-type byte in plaintext.
//!
//! # WAL Entry Layout
//!
//! ```text
//! Offset 0..8:   LSN (u64 LE)
//! Offset 8..20:  Timestamp (12 bytes, HybridTimestamp)
//! Offset 20..24: Checksum (u32 LE, CRC32)
//! Offset 24:     Op Type (u8 discriminant)
//! Offset 25+:    Operation-specific payload (variable length)
//! ```
//!
//! The first 25 bytes (header + op type) stay plaintext and are passed as AAD
//! (Additional Authenticated Data) so any tampering with them is detected during
//! decryption.

use std::sync::Arc;

use crate::encryption::Cipher;
use crate::encryption::error::EncryptionError;

/// Byte offset where the encrypted payload begins (24-byte header + 1-byte op type).
const WAL_HEADER_SIZE: usize = 25;

/// Encrypt the payload portion of a serialized WAL entry.
///
/// # Arguments
///
/// * `serialized` - Full serialized WAL entry: `[header:24][op_type:1][payload:N]`
/// * `cipher` - AEAD cipher to use for encryption
///
/// # Returns
///
/// A new buffer: `[header:24][op_type:1][encrypted_payload]` where
/// `encrypted_payload` is `N + cipher.overhead()` bytes.
///
/// # Errors
///
/// Returns [`EncryptionError::InvalidWalEntry`] if `serialized` is shorter than
/// 25 bytes (the minimum for header + op type with an empty payload).
pub fn encrypt_wal_payload(
    serialized: &[u8],
    cipher: &Arc<dyn Cipher>,
) -> Result<Vec<u8>, EncryptionError> {
    if serialized.len() < WAL_HEADER_SIZE {
        return Err(EncryptionError::InvalidWalEntry {
            expected: WAL_HEADER_SIZE,
            actual: serialized.len(),
        });
    }

    let header_and_op = &serialized[..WAL_HEADER_SIZE];
    let payload = &serialized[WAL_HEADER_SIZE..];

    // Use header + op type as AAD so tampering with them is detected.
    let encrypted_payload = cipher.encrypt(payload, header_and_op)?;

    let mut output = Vec::with_capacity(WAL_HEADER_SIZE + encrypted_payload.len());
    output.extend_from_slice(header_and_op);
    output.extend_from_slice(&encrypted_payload);
    Ok(output)
}

/// Decrypt the payload portion of a WAL entry encrypted by [`encrypt_wal_payload`].
///
/// # Arguments
///
/// * `encrypted_entry` - Encrypted WAL entry: `[header:24][op_type:1][encrypted_payload]`
/// * `cipher` - Same AEAD cipher used for encryption
///
/// # Returns
///
/// A new buffer: `[header:24][op_type:1][plaintext_payload]`.
///
/// # Errors
///
/// - [`EncryptionError::InvalidWalEntry`] if the entry is too short for the
///   header plus cipher overhead.
/// - [`EncryptionError::DecryptFailed`] if AAD verification fails (e.g. header
///   was tampered with) or the ciphertext is corrupted.
pub fn decrypt_wal_payload(
    encrypted_entry: &[u8],
    cipher: &Arc<dyn Cipher>,
) -> Result<Vec<u8>, EncryptionError> {
    let min_len = WAL_HEADER_SIZE + cipher.overhead();
    if encrypted_entry.len() < min_len {
        return Err(EncryptionError::InvalidWalEntry {
            expected: min_len,
            actual: encrypted_entry.len(),
        });
    }

    let header_and_op = &encrypted_entry[..WAL_HEADER_SIZE];
    let encrypted_payload = &encrypted_entry[WAL_HEADER_SIZE..];

    let plaintext = cipher.decrypt(encrypted_payload, header_and_op)?;

    let mut output = Vec::with_capacity(WAL_HEADER_SIZE + plaintext.len());
    output.extend_from_slice(header_and_op);
    output.extend_from_slice(&plaintext);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encryption::{Aes256GcmCipher, ChaCha20Poly1305Cipher};
    use rand::RngCore;
    use zeroize::Zeroizing;

    /// Build a fake serialized WAL entry with a recognizable payload.
    fn fake_entry(payload_len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; WAL_HEADER_SIZE + payload_len];
        // Fake LSN = 42
        buf[..8].copy_from_slice(&42u64.to_le_bytes());
        // Op type = 1 (CreateNode)
        buf[24] = 1;
        // Fill payload with a recognizable pattern
        for (i, b) in buf[WAL_HEADER_SIZE..].iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        buf
    }

    fn random_key() -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        rand::thread_rng().fill_bytes(key.as_mut());
        key
    }

    fn aes_cipher() -> Arc<dyn Cipher> {
        Arc::new(Aes256GcmCipher::new(&random_key()))
    }

    fn chacha_cipher() -> Arc<dyn Cipher> {
        Arc::new(ChaCha20Poly1305Cipher::new(&random_key()))
    }

    #[test]
    fn roundtrip_aes() {
        let cipher = aes_cipher();
        let entry = fake_entry(128);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();
        let decrypted = decrypt_wal_payload(&encrypted, &cipher).unwrap();

        assert_eq!(decrypted, entry);
    }

    #[test]
    fn roundtrip_chacha() {
        let cipher = chacha_cipher();
        let entry = fake_entry(128);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();
        let decrypted = decrypt_wal_payload(&encrypted, &cipher).unwrap();

        assert_eq!(decrypted, entry);
    }

    #[test]
    fn header_preserved_in_encrypted_output() {
        let cipher = aes_cipher();
        let entry = fake_entry(64);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();

        // First 25 bytes (header + op type) must be identical to the original.
        assert_eq!(&encrypted[..WAL_HEADER_SIZE], &entry[..WAL_HEADER_SIZE]);
    }

    #[test]
    fn payload_is_encrypted() {
        let cipher = aes_cipher();
        let entry = fake_entry(64);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();

        // Bytes after the header must differ from the original plaintext payload.
        assert_ne!(
            &encrypted[WAL_HEADER_SIZE..],
            &entry[WAL_HEADER_SIZE..],
            "payload should be encrypted (different from plaintext)"
        );
    }

    #[test]
    fn encrypted_is_larger() {
        let cipher = aes_cipher();
        let entry = fake_entry(64);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();

        assert_eq!(encrypted.len(), entry.len() + cipher.overhead());
    }

    #[test]
    fn tampered_header_fails_decryption() {
        let cipher = aes_cipher();
        let entry = fake_entry(64);

        let mut encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();
        // Flip a bit in the LSN portion of the header.
        encrypted[0] ^= 0xFF;

        let result = decrypt_wal_payload(&encrypted, &cipher);
        assert!(result.is_err(), "tampered header should fail AAD check");
    }

    #[test]
    fn empty_payload_roundtrips() {
        let cipher = aes_cipher();
        // Entry with exactly 25 bytes -- header + op type, zero-length payload.
        let entry = fake_entry(0);
        assert_eq!(entry.len(), WAL_HEADER_SIZE);

        let encrypted = encrypt_wal_payload(&entry, &cipher).unwrap();
        let decrypted = decrypt_wal_payload(&encrypted, &cipher).unwrap();

        assert_eq!(decrypted, entry);
    }

    #[test]
    fn too_short_entry_fails() {
        let cipher = aes_cipher();
        let short = vec![0u8; 10];

        let result = encrypt_wal_payload(&short, &cipher);
        assert!(matches!(
            result,
            Err(EncryptionError::InvalidWalEntry {
                expected: WAL_HEADER_SIZE,
                actual: 10
            })
        ));
    }
}
