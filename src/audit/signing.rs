//! Ed25519 signing / verifying key material for audit exports (Issue #3358).
//!
//! Design (mirrors the #3350/#3421 key-handling conventions):
//! - The private seed is held in a [`Zeroizing`] buffer and never logged; the
//!   [`Debug`] impl prints no key material.
//! - Key files are written atomically and chmodded `0600` on unix.
//! - Verification is asymmetric: a third party checks an artifact with ONLY the
//!   public key, so the operator cannot be a trusted party in the proof.

use crate::audit::error::{AuditError, AuditResult};
use crate::core::hex;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use rand::RngCore;
use rand::rngs::OsRng;
use std::path::Path;
use zeroize::Zeroizing;

/// Environment variable convention for supplying a signing key seed (hex).
pub const SIGNING_KEY_ENV: &str = "ALETHEIADB_AUDIT_SIGNING_KEY";

/// An Ed25519 signing key (32-byte seed) used to sign audit exports.
///
/// The seed is the operator's secret. It is never logged and is zeroized on drop
/// (via [`Zeroizing`]). Keep the corresponding [public key](AuditPublicKey) for
/// distribution to auditors.
pub struct AuditSigningKey {
    inner: SigningKey,
}

impl std::fmt::Debug for AuditSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Public-key fingerprint only — NEVER the secret seed.
        f.debug_struct("AuditSigningKey")
            .field("public_key", &self.public_key().to_hex())
            .finish()
    }
}

impl AuditSigningKey {
    /// Generate a fresh random signing key from the OS CSPRNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(seed.as_mut());
        Self {
            inner: SigningKey::from_bytes(&seed),
        }
    }

    /// Construct a signing key from a raw 32-byte seed.
    #[must_use]
    pub fn from_seed_bytes(seed: [u8; 32]) -> Self {
        let seed = Zeroizing::new(seed);
        Self {
            inner: SigningKey::from_bytes(&seed),
        }
    }

    /// Construct a signing key from a 64-char lowercase-hex seed.
    ///
    /// # Errors
    /// Returns [`AuditError::InvalidKey`] if the string is not 32 bytes of hex.
    pub fn from_seed_hex(s: &str) -> AuditResult<Self> {
        let bytes = hex::decode(s.trim())
            .ok_or_else(|| AuditError::InvalidKey("seed is not valid hex".to_string()))?;
        if bytes.len() != 32 {
            return Err(AuditError::InvalidKey(format!(
                "seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        seed.copy_from_slice(&bytes);
        Ok(Self {
            inner: SigningKey::from_bytes(&seed),
        })
    }

    /// Load a signing key from a file containing a hex seed (whitespace trimmed).
    ///
    /// # Errors
    /// Returns [`AuditError::Io`] on read failure, or [`AuditError::InvalidKey`]
    /// if the contents are not a valid 32-byte hex seed.
    pub fn from_file(path: impl AsRef<Path>) -> AuditResult<Self> {
        let contents = std::fs::read_to_string(path.as_ref())
            .map_err(|e| AuditError::Io(format!("reading signing key: {e}")))?;
        Self::from_seed_hex(&contents)
    }

    /// Load a signing key from an environment variable (hex seed).
    ///
    /// # Errors
    /// Returns [`AuditError::InvalidKey`] if the variable is unset or malformed.
    pub fn from_env(var: &str) -> AuditResult<Self> {
        let value = std::env::var(var).map_err(|_| {
            AuditError::InvalidKey(format!("environment variable {var} is not set"))
        })?;
        Self::from_seed_hex(&value)
    }

    /// Write the hex seed to a file with `0600` permissions on unix.
    ///
    /// The file is created with restrictive permissions from the start so the
    /// secret is never briefly world-readable.
    ///
    /// # Security (Issue #3351)
    /// The file is created with `create_new(true)` (`O_CREAT | O_EXCL`), so the
    /// call **fails if the path already exists** — an attacker cannot pre-plant a
    /// symlink or a sensitive file for us to follow and truncate. On unix we also
    /// add `O_NOFOLLOW` as defense-in-depth so the final path component is never a
    /// followed symlink. The previous `create(true).truncate(true)` behavior
    /// silently followed symlinks and truncated whatever it found.
    ///
    /// # Errors
    /// Returns [`AuditError::Io`] on write failure, including when the target path
    /// already exists (write over refused) or is a symlink.
    pub fn write_to_file(&self, path: impl AsRef<Path>) -> AuditResult<()> {
        let path = path.as_ref();
        let seed = Zeroizing::new(self.inner.to_bytes());
        let hex_seed = Zeroizing::new(hex::encode(seed.as_ref()));

        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        // create_new => O_CREAT | O_EXCL: refuse to open a pre-existing path
        // (regular file OR symlink), so we never follow/truncate attacker-planted
        // content. No truncate flag: we only ever create fresh.
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
            // Belt-and-suspenders: also refuse to follow a symlink at the final
            // component. libc supplies the correct per-platform O_NOFOLLOW value.
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path).map_err(|e| {
            AuditError::Io(format!(
                "creating signing key file (refused: path must not already exist and must not be a symlink): {e}"
            ))
        })?;
        file.write_all(hex_seed.as_bytes())
            .map_err(|e| AuditError::Io(format!("writing signing key file: {e}")))?;
        file.write_all(b"\n")
            .map_err(|e| AuditError::Io(format!("writing signing key file: {e}")))?;
        Ok(())
    }

    /// The public key corresponding to this signing key.
    #[must_use]
    pub fn public_key(&self) -> AuditPublicKey {
        AuditPublicKey {
            inner: self.inner.verifying_key(),
        }
    }

    /// Sign a 32-byte chain root, returning the raw 64-byte signature.
    pub(crate) fn sign_root(&self, root: &[u8; 32]) -> [u8; 64] {
        use ed25519_dalek::Signer as _;
        self.inner.sign(root).to_bytes()
    }
}

/// An Ed25519 public (verifying) key an auditor uses to check an artifact.
#[derive(Clone)]
pub struct AuditPublicKey {
    inner: VerifyingKey,
}

impl std::fmt::Debug for AuditPublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditPublicKey")
            .field("hex", &self.to_hex())
            .finish()
    }
}

impl AuditPublicKey {
    /// Lowercase-hex encoding of the 32-byte public key.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.inner.as_bytes())
    }

    /// Parse a public key from its 64-char lowercase-hex encoding.
    ///
    /// # Errors
    /// Returns [`AuditError::InvalidPublicKey`] if not a valid 32-byte key.
    pub fn from_hex(s: &str) -> AuditResult<Self> {
        let bytes = hex::decode(s.trim())
            .ok_or_else(|| AuditError::InvalidPublicKey("not valid hex".to_string()))?;
        if bytes.len() != 32 {
            return Err(AuditError::InvalidPublicKey(format!(
                "public key must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let inner = VerifyingKey::from_bytes(&arr)
            .map_err(|e| AuditError::InvalidPublicKey(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Verify a 64-byte signature over a 32-byte chain root.
    pub(crate) fn verify_root(&self, root: &[u8; 32], signature: &[u8; 64]) -> AuditResult<()> {
        use ed25519_dalek::Verifier as _;
        let sig = Signature::from_bytes(signature);
        self.inner
            .verify(root, &sig)
            .map_err(|e| AuditError::SignatureInvalid(e.to_string()))
    }
}
