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

use std::sync::{Arc, RwLock};

use crate::encryption::Cipher;
use crate::encryption::error::EncryptionError;

/// Byte offset where the encrypted payload begins (24-byte header + 1-byte op type).
const WAL_HEADER_SIZE: usize = 25;

/// The `key_version` a fresh (never-rotated) encrypted WAL stamps into its
/// keyversioned segment headers (Issue #3617). Mirrors the index keyring's
/// `ENC_INDEX_KEY_VERSION_V1` starting point so the two layers number their
/// first generation identically.
pub const INITIAL_WAL_KEY_VERSION: u32 = 1;

// ── WAL key ring (dual-generation rotation, Issue #3617) ─────────────
//
// A full-MEK key rotation re-keys the WAL layer WITHOUT a bulk rewrite: new
// appends are written under the NEW WAL DEK (in `KEYVERSIONED` segments
// stamping the new `key_version`), while legacy/old-DEK segments still on disk
// replay under the OLD DEK until they are truncated. To decrypt correctly the
// recovery reader must therefore be able to hold BOTH generations at once and
// select per segment. The `WalKeyring` is that holder — the WAL analogue of the
// index [`IndexKeyring`](crate::storage::index_persistence::common::IndexKeyring),
// deliberately mirroring its shape (small `Vec` of generations, a `current`
// stamped into new segments, and a single-cipher `match_any` back-compat mode).

#[derive(Clone)]
struct WalKeyGeneration {
    key_version: u32,
    cipher: Arc<dyn Cipher>,
}

struct WalKeyringInner {
    /// All live generations (1 before rotation, 2 during, 1 after). Small.
    generations: Vec<WalKeyGeneration>,
    /// Version stamped into freshly written segments (the newest generation).
    current_version: u32,
    /// Back-compat mode: a lone generation created from a single cipher
    /// decrypts ANY segment (any `key_version`, or a legacy header carrying
    /// none) with that one cipher. This is the never-rotated steady state and,
    /// crucially, the post-provider-switch state: after a completed rotation the
    /// operator reopens under the new key alone, so the single new-DEK keyring
    /// must still read the new-DEK segments regardless of the `key_version` they
    /// were stamped with during the rotation.
    match_any: bool,
}

/// A cheaply-cloneable, shared-mutable set of WAL DEK ciphers addressed by
/// `key_version` (Issue #3617).
///
/// Clones share the same underlying state, so the rotation driver can advance
/// the current generation (making new appends stamp + encrypt under the new
/// DEK) while the flush coordinator and recovery reader holding clones observe
/// the change immediately. Never exposes key material; `Debug` is redacted.
#[derive(Clone)]
pub struct WalKeyring {
    inner: Arc<RwLock<WalKeyringInner>>,
}

impl std::fmt::Debug for WalKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material — only opaque generation metadata.
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("WalKeyring")
            .field("generations", &inner.generations.len())
            .field("current_version", &inner.current_version)
            .field("match_any", &inner.match_any)
            .finish()
    }
}

impl WalKeyring {
    /// A single-generation keyring from one cipher (the non-rotation path).
    ///
    /// Reads decrypt any segment (any header `key_version`, or a legacy header
    /// with none) with this cipher and writes stamp [`INITIAL_WAL_KEY_VERSION`].
    pub fn single(cipher: Arc<dyn Cipher>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(WalKeyringInner {
                generations: vec![WalKeyGeneration {
                    key_version: INITIAL_WAL_KEY_VERSION,
                    cipher,
                }],
                current_version: INITIAL_WAL_KEY_VERSION,
                match_any: true,
            })),
        }
    }

    /// A single-generation WAL keyring pinned to an explicit `key_version`
    /// (Issue #488 version-provisioning). Reads decrypt every segment with this
    /// one cipher (`match_any`, byte-identical to [`Self::single`]); only the
    /// write-stamp / reported [`current_version`](Self::current_version) is
    /// pinned to `key_version`. The durable `open()` path builds the WAL keyring
    /// at the max on-disk segment `key_version` so new segments stamp the real
    /// version instead of a stale [`INITIAL_WAL_KEY_VERSION`], keeping the WAL
    /// and index layers in lockstep after a rotate-then-reopen.
    pub fn single_versioned(cipher: Arc<dyn Cipher>, key_version: u32) -> Self {
        Self {
            inner: Arc::new(RwLock::new(WalKeyringInner {
                generations: vec![WalKeyGeneration {
                    key_version,
                    cipher,
                }],
                current_version: key_version,
                match_any: true,
            })),
        }
    }

    /// The current (write) cipher and the `key_version` it stamps into new
    /// segment headers, or `None` if the keyring somehow holds no generation.
    pub fn current(&self) -> Option<(Arc<dyn Cipher>, u32)> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let v = inner.current_version;
        inner
            .generations
            .iter()
            .find(|g| g.key_version == v)
            .map(|g| (g.cipher.clone(), v))
    }

    /// The version freshly written segments are stamped with.
    pub fn current_version(&self) -> u32 {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current_version
    }

    /// Resolve the decryption cipher for a segment, given the `key_version` read
    /// from its header (`Some` for a `KEYVERSIONED` segment, `None` for a legacy
    /// segment that carries no key-version field).
    ///
    /// * `match_any` (single-cipher) keyring → the sole cipher for every
    ///   segment (never-rotated / post-switch steady state).
    /// * strict keyring, legacy segment (`None`) → the OLDEST (minimum-version,
    ///   i.e. pre-rotation) generation — the DEK legacy segments were written
    ///   under before the rotation advanced the generation.
    /// * strict keyring, `Some(kv)` → the generation stamped `kv`, or `None`
    ///   when no such generation is held (a genuine wrong/absent key surfaces as
    ///   a loud AEAD failure downstream rather than silent wrong data).
    pub fn cipher_for_segment(&self, key_version: Option<u32>) -> Option<Arc<dyn Cipher>> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        if inner.match_any {
            return inner.generations.first().map(|g| g.cipher.clone());
        }
        match key_version {
            Some(kv) => inner
                .generations
                .iter()
                .find(|g| g.key_version == kv)
                .map(|g| g.cipher.clone()),
            None => inner
                .generations
                .iter()
                .min_by_key(|g| g.key_version)
                .map(|g| g.cipher.clone()),
        }
    }

    /// Whether a generation with `key_version` is currently held (a `match_any`
    /// single keyring holds every version).
    ///
    /// Test-only for now (Issue #3617 PR2): no production path consults it —
    /// segment dispatch goes through [`cipher_for_segment`](Self::cipher_for_segment).
    /// `#[cfg(test)]`-gated rather than removed so PR3 can re-expose it if a
    /// production caller lands.
    #[cfg(test)]
    pub fn has_version(&self, key_version: u32) -> bool {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.match_any
            || inner
                .generations
                .iter()
                .any(|g| g.key_version == key_version)
    }

    /// Add a new generation and make it current. Switches the keyring to strict
    /// per-version dispatch (leaving `match_any`). Idempotent for a version
    /// already present (its cipher is replaced). Used by the rotation driver to
    /// install the new WAL DEK before force-rolling the active segment.
    pub fn add_generation(&self, key_version: u32, cipher: Arc<dyn Cipher>) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.match_any = false;
        inner.generations.retain(|g| g.key_version != key_version);
        inner.generations.push(WalKeyGeneration {
            key_version,
            cipher,
        });
        inner.current_version = key_version;
    }

    /// Number of live generations (test/introspection helper; never leaks keys).
    #[cfg(test)]
    pub fn generation_count(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .generations
            .len()
    }
}

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

    // ── WalKeyring (Issue #3617) ─────────────────────────────────────

    #[test]
    fn keyring_single_matches_any_version() {
        let cipher = aes_cipher();
        let ring = WalKeyring::single(Arc::clone(&cipher));
        assert_eq!(ring.current_version(), INITIAL_WAL_KEY_VERSION);
        assert_eq!(ring.generation_count(), 1);
        // A single-cipher keyring decrypts any segment: a legacy header (None),
        // its own version, or an unrelated version stamped during a since-
        // completed rotation (post-provider-switch state).
        assert!(ring.cipher_for_segment(None).is_some());
        assert!(
            ring.cipher_for_segment(Some(INITIAL_WAL_KEY_VERSION))
                .is_some()
        );
        assert!(ring.cipher_for_segment(Some(999)).is_some());
        assert!(ring.has_version(7));
    }

    #[test]
    fn keyring_dual_generation_dispatch() {
        let old = aes_cipher();
        let new = aes_cipher();
        let ring = WalKeyring::single(Arc::clone(&old));
        ring.add_generation(2, Arc::clone(&new));
        // Now strict: current stamps the new version.
        assert_eq!(ring.current_version(), 2);
        assert_eq!(ring.generation_count(), 2);
        let (cur, v) = ring.current().unwrap();
        assert_eq!(v, 2);
        assert!(Arc::ptr_eq(&cur, &new));
        // Legacy segment (no key_version) resolves to the pre-rotation (oldest)
        // generation.
        assert!(Arc::ptr_eq(&ring.cipher_for_segment(None).unwrap(), &old));
        // Keyversioned segments resolve to their exact generation.
        assert!(Arc::ptr_eq(
            &ring.cipher_for_segment(Some(1)).unwrap(),
            &old
        ));
        assert!(Arc::ptr_eq(
            &ring.cipher_for_segment(Some(2)).unwrap(),
            &new
        ));
        // An unheld version is a loud miss (no silent wrong-key), not a fallback.
        assert!(ring.cipher_for_segment(Some(3)).is_none());
    }

    #[test]
    fn keyring_add_generation_is_idempotent_for_same_version() {
        let old = aes_cipher();
        let new = aes_cipher();
        let ring = WalKeyring::single(Arc::clone(&old));
        ring.add_generation(2, Arc::clone(&new));
        ring.add_generation(2, Arc::clone(&new));
        assert_eq!(ring.generation_count(), 2);
        assert_eq!(ring.current_version(), 2);
    }

    #[test]
    fn keyring_debug_redacts_key_material() {
        let ring = WalKeyring::single(aes_cipher());
        let rendered = format!("{ring:?}");
        assert!(rendered.contains("WalKeyring"));
        assert!(rendered.contains("current_version"));
        // Never renders the cipher/algorithm/key bytes.
        assert!(!rendered.to_lowercase().contains("aes"));
    }
}
