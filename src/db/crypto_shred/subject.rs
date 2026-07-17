//! Subject identifiers and per-subject key material (Issue #3359, slice PR-1a).
//!
//! A [`SubjectId`] is a caller-chosen **identifier** grouping the entities /
//! property keys that share one erasure fate. It is not a secret — its `Display`
//! and `Debug` are allowed to print the id.
//!
//! A [`SubjectKey`] is the random 32-byte per-subject data-encryption key (DEK).
//! It **is** a secret: the bytes live in a [`Zeroizing`] buffer, its `Debug`
//! redacts, and it exposes no `Serialize`/`Display` of the raw bytes.

use zeroize::Zeroizing;

use super::error::CryptoShredError;

/// Maximum accepted length (in bytes) of a subject identifier.
///
/// Bounded to keep the durable keyring compact and to reject accidental
/// large-blob ids; 256 bytes is far more than any real subject id needs.
pub const MAX_SUBJECT_ID_LEN: usize = 256;

/// A validated erasure-subject identifier.
///
/// Construction validates the id: non-empty, `<= MAX_SUBJECT_ID_LEN` bytes, and
/// free of control characters (so it never corrupts logs / breadcrumb files).
/// The identifier is **not** secret — it appears in attestations, breadcrumbs,
/// and error `details` so callers can act on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubjectId(String);

impl SubjectId {
    /// Validate and wrap a subject identifier.
    ///
    /// # Errors
    /// [`CryptoShredError::InvalidArgument`] if the id is empty, too long, or
    /// contains control characters.
    pub fn new(id: impl Into<String>) -> Result<Self, CryptoShredError> {
        let id = id.into();
        if id.is_empty() {
            return Err(CryptoShredError::InvalidArgument(
                "subject id must not be empty".to_string(),
            ));
        }
        if id.len() > MAX_SUBJECT_ID_LEN {
            return Err(CryptoShredError::InvalidArgument(format!(
                "subject id must be <= {MAX_SUBJECT_ID_LEN} bytes"
            )));
        }
        if id.chars().any(|c| c.is_control()) {
            return Err(CryptoShredError::InvalidArgument(
                "subject id must not contain control characters".to_string(),
            ));
        }
        Ok(Self(id))
    }

    /// The identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The identifier's bytes (used as AEAD AAD when wrapping the subject DEK).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Consume into the inner `String`.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Display for SubjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Length of a subject data-encryption key in bytes.
pub const SUBJECT_KEY_LEN: usize = 32;

/// A random per-subject data-encryption key (DEK).
///
/// # Security
/// The 32 raw key bytes are held in a [`Zeroizing`] buffer (wiped on drop). The
/// [`Debug`] impl **redacts** — it never prints the bytes or a hex encoding —
/// and the type intentionally implements neither `Serialize` nor `Display`, so
/// no code path can accidentally emit the key into a log, error, or file. Only
/// the wrapped (encrypted) form is ever persisted (see
/// [`super::keyring::WrappedKey`]).
#[derive(Clone)]
pub struct SubjectKey(Zeroizing<[u8; SUBJECT_KEY_LEN]>);

impl SubjectKey {
    /// Generate a fresh random subject key from the OS CSPRNG.
    ///
    /// This is a **random independent** key, not derived from the MEK — that is
    /// the crux of crypto-shred: destroying it is irreversible even for a holder
    /// of the MEK (see the design doc's rejection of `HKDF(MEK, subject_id)`).
    #[must_use]
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = Zeroizing::new([0u8; SUBJECT_KEY_LEN]);
        rand::rngs::OsRng.fill_bytes(bytes.as_mut());
        Self(bytes)
    }

    /// Wrap raw key bytes (used after unwrapping from the keyring).
    #[must_use]
    pub fn from_bytes(bytes: [u8; SUBJECT_KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Borrow the raw key bytes (crate-internal; for wrapping/cipher build only).
    #[must_use]
    pub(crate) fn expose_bytes(&self) -> &[u8; SUBJECT_KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for SubjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER print key material.
        f.write_str("SubjectKey(<redacted>)")
    }
}
