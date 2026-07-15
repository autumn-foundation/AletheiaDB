//! Passphrase-wrapped master-key files (Issue #3587).
//!
//! A passphrase key file stores the 32-byte Master Encryption Key (MEK)
//! **wrapped** (AES-256-GCM encrypted) under a key derived from a
//! human-supplied passphrase via a memory-/CPU-hard KDF. The passphrase itself
//! is never written to disk; only a salt, KDF parameters, and the AEAD-sealed
//! MEK are persisted. This lets an operator carry a single secret in their head
//! (or a secrets manager) instead of a raw 256-bit key file.
//!
//! # File format (self-describing binary)
//!
//! ```text
//! offset  size  field
//! 0       4     magic         = b"AEKF"
//! 4       1     format_version = 1
//! 5       1     kdf_id         (1 = Argon2id, 2 = PBKDF2-HMAC-SHA256)
//! 6       16    kdf_params     (fixed 16 bytes, interpreted per kdf_id)
//! 22      16    salt
//! 38      12    aes-gcm nonce
//! 50      32    ciphertext     (AES-256-GCM sealed MEK)
//! 82      16    aes-gcm tag
//! total   98 bytes
//! ```
//!
//! `kdf_params` layout:
//! - Argon2id: `m_cost: u32 | t_cost: u32 | p_cost: u32 | reserved: u32` (BE)
//! - PBKDF2:   `iterations: u32 | reserved [12 bytes]` (BE)
//!
//! # Security
//!
//! - The passphrase lives only in a [`Zeroizing`] buffer and is never logged,
//!   returned in an error string, or exposed via `Debug`.
//! - The recovered MEK is a [`Zeroizing<[u8; 32]>`], erased on drop.
//! - A wrong passphrase derives the wrong wrapping key, so the AES-GCM tag check
//!   fails and [`get_mek`](PassphraseFileKeyProvider::get_mek) returns
//!   [`KeyProviderError::AccessDenied`] -- it never panics.
//! - On Unix the generated file is created `0600` before any bytes are written.

use std::fmt;
use std::path::{Path, PathBuf};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce as AesNonce};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::encryption::KeyProviderError;
use crate::encryption::key_provider::KeyProvider;

/// Magic bytes identifying a passphrase-wrapped key file.
const MAGIC: &[u8; 4] = b"AEKF";
/// Current on-disk format version.
const FORMAT_VERSION: u8 = 1;
/// KDF identifier: Argon2id.
const KDF_ARGON2ID: u8 = 1;
/// KDF identifier: PBKDF2-HMAC-SHA256.
const KDF_PBKDF2: u8 = 2;

const KDF_PARAMS_LEN: usize = 16;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const CT_LEN: usize = 32;
const TAG_LEN: usize = 16;

const OFF_MAGIC: usize = 0;
const OFF_FORMAT: usize = 4;
const OFF_KDF_ID: usize = 5;
const OFF_KDF_PARAMS: usize = 6;
const OFF_SALT: usize = OFF_KDF_PARAMS + KDF_PARAMS_LEN; // 22
const OFF_NONCE: usize = OFF_SALT + SALT_LEN; // 38
const OFF_CT: usize = OFF_NONCE + NONCE_LEN; // 50
const OFF_TAG: usize = OFF_CT + CT_LEN; // 82
const FILE_LEN: usize = OFF_TAG + TAG_LEN; // 98

/// Key-derivation function used to derive the wrapping key from a passphrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassphraseKdf {
    /// Argon2id (memory-hard, preferred).
    Argon2id {
        /// Memory cost in KiB.
        m_cost: u32,
        /// Iteration (time) cost.
        t_cost: u32,
        /// Degree of parallelism.
        p_cost: u32,
    },
    /// PBKDF2-HMAC-SHA256 (widely interoperable fallback).
    Pbkdf2 {
        /// Iteration count.
        iterations: u32,
    },
}

impl PassphraseKdf {
    /// Secure defaults for interactive use (OWASP-aligned).
    #[must_use]
    pub fn argon2id_default() -> Self {
        // 19 MiB, 2 passes, 1 lane.
        Self::Argon2id {
            m_cost: 19 * 1024,
            t_cost: 2,
            p_cost: 1,
        }
    }

    /// Secure default for the PBKDF2 fallback.
    #[must_use]
    pub fn pbkdf2_default() -> Self {
        Self::Pbkdf2 {
            iterations: 600_000,
        }
    }

    fn kdf_id(self) -> u8 {
        match self {
            Self::Argon2id { .. } => KDF_ARGON2ID,
            Self::Pbkdf2 { .. } => KDF_PBKDF2,
        }
    }

    fn encode_params(self) -> [u8; KDF_PARAMS_LEN] {
        let mut out = [0u8; KDF_PARAMS_LEN];
        match self {
            Self::Argon2id {
                m_cost,
                t_cost,
                p_cost,
            } => {
                out[0..4].copy_from_slice(&m_cost.to_be_bytes());
                out[4..8].copy_from_slice(&t_cost.to_be_bytes());
                out[8..12].copy_from_slice(&p_cost.to_be_bytes());
            }
            Self::Pbkdf2 { iterations } => {
                out[0..4].copy_from_slice(&iterations.to_be_bytes());
            }
        }
        out
    }

    fn decode(kdf_id: u8, params: &[u8; KDF_PARAMS_LEN]) -> Result<Self, KeyProviderError> {
        let u32_at = |i: usize| {
            let mut b = [0u8; 4];
            b.copy_from_slice(&params[i..i + 4]);
            u32::from_be_bytes(b)
        };
        match kdf_id {
            KDF_ARGON2ID => Ok(Self::Argon2id {
                m_cost: u32_at(0),
                t_cost: u32_at(4),
                p_cost: u32_at(8),
            }),
            KDF_PBKDF2 => Ok(Self::Pbkdf2 {
                iterations: u32_at(0),
            }),
            other => Err(KeyProviderError::InvalidKeyFormat(format!(
                "unknown KDF id {other}"
            ))),
        }
    }

    /// Derive a 32-byte wrapping key from the passphrase and salt.
    fn derive(
        self,
        passphrase: &[u8],
        salt: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let mut key = Zeroizing::new([0u8; 32]);
        match self {
            Self::Argon2id {
                m_cost,
                t_cost,
                p_cost,
            } => {
                use argon2::{Algorithm, Argon2, Params, Version};
                let params = Params::new(m_cost, t_cost, p_cost, Some(32)).map_err(|e| {
                    KeyProviderError::InvalidKeyFormat(format!("invalid Argon2 params: {e}"))
                })?;
                let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
                argon
                    .hash_password_into(passphrase, salt, key.as_mut())
                    .map_err(|e| {
                        // The error carries only parameter/length context, never
                        // the passphrase bytes.
                        KeyProviderError::Provider(Box::new(std::io::Error::other(format!(
                            "Argon2 derivation failed: {e}"
                        ))))
                    })?;
            }
            Self::Pbkdf2 { iterations } => {
                // pbkdf2 0.12 is built on the digest 0.10 ecosystem; pair it with
                // the sha2 0.10 copy aliased as `sha2_pbkdf2`.
                pbkdf2::pbkdf2_hmac::<sha2_pbkdf2::Sha256>(
                    passphrase,
                    salt,
                    iterations,
                    key.as_mut(),
                );
            }
        }
        Ok(key)
    }
}

/// Reads the MEK from a passphrase-wrapped key file.
///
/// The passphrase is supplied at construction and held only in a zeroizing
/// buffer. `Debug` deliberately omits it.
pub struct PassphraseFileKeyProvider {
    path: PathBuf,
    passphrase: Zeroizing<String>,
}

impl fmt::Debug for PassphraseFileKeyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NEVER expose the passphrase.
        f.debug_struct("PassphraseFileKeyProvider")
            .field("path", &self.path)
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

impl PassphraseFileKeyProvider {
    /// Create a provider that unwraps the MEK at `path` using `passphrase`.
    pub fn new(path: impl Into<PathBuf>, passphrase: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            passphrase: Zeroizing::new(passphrase.into()),
        }
    }

    fn unwrap_key(&self, raw: &[u8]) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        if raw.len() != FILE_LEN {
            return Err(KeyProviderError::InvalidKeyFormat(format!(
                "expected {FILE_LEN}-byte passphrase key file, got {}",
                raw.len()
            )));
        }
        if &raw[OFF_MAGIC..OFF_MAGIC + 4] != MAGIC {
            return Err(KeyProviderError::InvalidKeyFormat(
                "bad magic (not a passphrase key file)".to_string(),
            ));
        }
        let format_version = raw[OFF_FORMAT];
        if format_version != FORMAT_VERSION {
            return Err(KeyProviderError::InvalidKeyFormat(format!(
                "unsupported format version {format_version}"
            )));
        }
        let kdf_id = raw[OFF_KDF_ID];
        let mut params = [0u8; KDF_PARAMS_LEN];
        params.copy_from_slice(&raw[OFF_KDF_PARAMS..OFF_SALT]);
        let kdf = PassphraseKdf::decode(kdf_id, &params)?;

        let salt = &raw[OFF_SALT..OFF_NONCE];
        let nonce = &raw[OFF_NONCE..OFF_CT];
        // ciphertext || tag is what aes-gcm's `decrypt` expects.
        let sealed = &raw[OFF_CT..FILE_LEN];

        let wrapping = kdf.derive(self.passphrase.as_bytes(), salt)?;
        let cipher = Aes256Gcm::new_from_slice(wrapping.as_ref()).map_err(|_| {
            KeyProviderError::InvalidKeyFormat("invalid wrapping key length".to_string())
        })?;
        let plaintext = cipher
            .decrypt(AesNonce::from_slice(nonce), sealed)
            .map_err(|_| {
                // A GCM tag mismatch is the expected outcome of a wrong
                // passphrase or tampered file. Return AccessDenied WITHOUT any
                // key/passphrase material.
                KeyProviderError::AccessDenied(
                    "passphrase key unwrap failed (wrong passphrase or corrupt file)".to_string(),
                )
            })?;
        if plaintext.len() != 32 {
            return Err(KeyProviderError::InvalidKeyFormat(format!(
                "unwrapped key is {} bytes, expected 32",
                plaintext.len()
            )));
        }
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&plaintext);
        Ok(key)
    }
}

impl KeyProvider for PassphraseFileKeyProvider {
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let raw = std::fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                KeyProviderError::KeyNotFound
            } else {
                KeyProviderError::Io(e)
            }
        })?;
        self.unwrap_key(&raw)
    }

    fn provider_name(&self) -> &str {
        "passphrase"
    }

    fn health_check(&self) -> Result<(), KeyProviderError> {
        self.get_mek().map(|_| ())
    }
}

/// Generate a new random MEK, wrap it under `passphrase`, and write a
/// passphrase key file at `path` using the default Argon2id KDF.
///
/// Returns the generated (plaintext) MEK so a caller can immediately use it.
///
/// # Security
///
/// On Unix the file is created `0600` before any bytes are written. When
/// `overwrite` is `false` an existing file is never clobbered (`O_EXCL`).
///
/// # Errors
///
/// Returns [`KeyProviderError`] if key derivation, encryption, or file writing
/// fails, or (when `overwrite` is `false`) if a file already exists.
pub fn generate_passphrase_key_file(
    path: &Path,
    passphrase: &str,
    overwrite: bool,
) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    generate_passphrase_key_file_with_kdf(
        path,
        passphrase,
        overwrite,
        PassphraseKdf::argon2id_default(),
    )
}

/// Like [`generate_passphrase_key_file`] but with an explicit KDF choice.
///
/// Primarily used to select PBKDF2 or fast test parameters.
///
/// # Errors
///
/// See [`generate_passphrase_key_file`].
pub fn generate_passphrase_key_file_with_kdf(
    path: &Path,
    passphrase: &str,
    overwrite: bool,
    kdf: PassphraseKdf,
) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    use std::io::Write;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    // Generate MEK, salt, and nonce.
    let mut mek = Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(mek.as_mut());
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);

    // Derive wrapping key and seal the MEK.
    let wrapping = kdf.derive(passphrase.as_bytes(), &salt)?;
    let cipher = Aes256Gcm::new_from_slice(wrapping.as_ref()).map_err(|_| {
        KeyProviderError::InvalidKeyFormat("invalid wrapping key length".to_string())
    })?;
    let sealed = cipher
        .encrypt(AesNonce::from_slice(&nonce), &mek[..])
        .map_err(|e| {
            KeyProviderError::Provider(Box::new(std::io::Error::other(format!(
                "key wrap failed: {e}"
            ))))
        })?;
    // sealed = ciphertext(32) || tag(16)
    if sealed.len() != CT_LEN + TAG_LEN {
        return Err(KeyProviderError::InvalidKeyFormat(format!(
            "unexpected sealed length {}",
            sealed.len()
        )));
    }

    // Assemble the file buffer.
    let mut buf = Zeroizing::new(Vec::with_capacity(FILE_LEN));
    buf.extend_from_slice(MAGIC);
    buf.push(FORMAT_VERSION);
    buf.push(kdf.kdf_id());
    buf.extend_from_slice(&kdf.encode_params());
    buf.extend_from_slice(&salt);
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&sealed);
    debug_assert_eq!(buf.len(), FILE_LEN);

    // Create the file 0600 at creation time; O_EXCL unless overwriting.
    if overwrite {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(&buf)?;
    f.flush()?;

    Ok(mek)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_argon() -> PassphraseKdf {
        // Tiny cost so tests are fast; still exercises the Argon2id code path.
        PassphraseKdf::Argon2id {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn fast_pbkdf2() -> PassphraseKdf {
        PassphraseKdf::Pbkdf2 { iterations: 1000 }
    }

    #[test]
    fn round_trip_argon2id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("argon.aekf");

        let generated = generate_passphrase_key_file_with_kdf(
            &path,
            "correct horse battery",
            true,
            fast_argon(),
        )
        .unwrap();

        let provider = PassphraseFileKeyProvider::new(&path, "correct horse battery");
        let loaded = provider.get_mek().unwrap();
        assert_eq!(generated.as_ref(), loaded.as_ref());
        assert!(provider.health_check().is_ok());
        assert_eq!(provider.provider_name(), "passphrase");
    }

    #[test]
    fn round_trip_pbkdf2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pbkdf2.aekf");

        let generated =
            generate_passphrase_key_file_with_kdf(&path, "hunter2-is-bad", true, fast_pbkdf2())
                .unwrap();

        let provider = PassphraseFileKeyProvider::new(&path, "hunter2-is-bad");
        let loaded = provider.get_mek().unwrap();
        assert_eq!(generated.as_ref(), loaded.as_ref());
    }

    #[test]
    fn wrong_passphrase_is_access_denied_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong.aekf");
        generate_passphrase_key_file_with_kdf(&path, "the-right-one", true, fast_argon()).unwrap();

        let provider = PassphraseFileKeyProvider::new(&path, "the-WRONG-one");
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::AccessDenied(_)),
            "expected AccessDenied, got: {err}"
        );
        // The error text must not leak the passphrase.
        let text = err.to_string();
        assert!(!text.contains("the-WRONG-one"));
        assert!(!text.contains("the-right-one"));
    }

    #[test]
    fn truncated_file_is_invalid_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trunc.aekf");
        std::fs::write(&path, b"AEKF short").unwrap();

        let provider = PassphraseFileKeyProvider::new(&path, "whatever");
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "expected InvalidKeyFormat, got: {err}"
        );
    }

    #[test]
    fn garbage_full_length_file_is_invalid_or_denied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.aekf");
        // Right length, wrong magic.
        std::fs::write(&path, vec![0xABu8; FILE_LEN]).unwrap();

        let provider = PassphraseFileKeyProvider::new(&path, "whatever");
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "expected InvalidKeyFormat for bad magic, got: {err}"
        );
    }

    #[test]
    fn missing_file_is_key_not_found() {
        let provider = PassphraseFileKeyProvider::new("/nonexistent/pp.aekf", "x");
        let err = provider.get_mek().unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyNotFound), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn generated_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("perm.aekf");
        generate_passphrase_key_file_with_kdf(&path, "pw", true, fast_argon()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    #[test]
    fn no_overwrite_refuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exists.aekf");
        std::fs::write(&path, b"pre-existing").unwrap();
        let err =
            generate_passphrase_key_file_with_kdf(&path, "pw", false, fast_argon()).unwrap_err();
        assert!(matches!(err, KeyProviderError::Io(_)), "got: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), b"pre-existing");
    }

    #[test]
    fn debug_does_not_leak_passphrase() {
        let provider = PassphraseFileKeyProvider::new("/tmp/k.aekf", "super-secret-phrase");
        let dbg = format!("{provider:?}");
        assert!(
            !dbg.contains("super-secret-phrase"),
            "debug leaked passphrase: {dbg}"
        );
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn get_key_version_returns_sole_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.aekf");
        let generated =
            generate_passphrase_key_file_with_kdf(&path, "pw", true, fast_argon()).unwrap();
        let provider = PassphraseFileKeyProvider::new(&path, "pw");
        // Single-version provider: any version -> the sole key.
        let v = provider.get_key_version(99).unwrap();
        assert_eq!(v.as_ref(), generated.as_ref());
    }
}
