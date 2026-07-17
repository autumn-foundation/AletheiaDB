//! Structured errors for the crypto-shred subsystem (Issue #3359, slice PR-1a).
//!
//! Every variant maps to one of the #3234 structured error categories via
//! [`CryptoShredError::code`]. **No variant embeds key material or plaintext** —
//! messages carry only references (subject ids, key versions, counts).

/// Errors returned by the crypto-shred designation / erasure API.
#[derive(Debug, thiserror::Error)]
pub enum CryptoShredError {
    /// A caller argument is malformed (bad subject id, empty designation, ...).
    #[error("{0}")]
    InvalidArgument(String),

    /// The requested subject is not designated (erasure requires designation).
    /// Maps to `FAILED_PRECONDITION` (AC6).
    #[error("subject '{0}' is not designated; designation is required before erasure")]
    NotDesignated(String),

    /// Encryption is not configured on this database; crypto-shred requires a
    /// key provider (aligns with the #477-491 encryption-at-rest config).
    /// Maps to `FAILED_PRECONDITION`.
    #[error("encryption is not configured; crypto-shred requires an encryption key provider")]
    EncryptionNotConfigured,

    /// The subject's key has been destroyed — its payload is permanently
    /// unrecoverable. Maps to `FAILED_PRECONDITION`.
    #[error("subject '{0}' has been erased; its key was destroyed and cannot be recovered")]
    SubjectErased(String),

    /// A subject already exists (designation conflict) or a re-designation would
    /// change immutable attributes. Maps to `CONFLICT`.
    #[error("{0}")]
    Conflict(String),

    /// Key sourcing / derivation / AEAD failure (wrong key, corrupt wrap blob).
    /// Maps to `INTERNAL`. The message never contains key bytes or plaintext.
    #[error("crypto-shred key operation failed: {0}")]
    Crypto(String),

    /// Durable I/O failure reading or writing the keyring / breadcrumb.
    /// Maps to `INTERNAL`.
    #[error("crypto-shred durable I/O failed: {0}")]
    Io(String),

    /// The durable keyring file is present but corrupt/undecodable. Fail-closed:
    /// startup aborts rather than silently starting with an empty registry (the
    /// opposite of the tolerant snapshot-quarantine policy), so an erased subject
    /// can never be silently re-exposed. Maps to `INTERNAL`.
    #[error("crypto-shred keyring present but corrupt: {0}")]
    KeyringCorrupt(String),
}

impl CryptoShredError {
    /// The #3234 structured error code for this error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            CryptoShredError::InvalidArgument(_) => "INVALID_ARGUMENT",
            CryptoShredError::NotDesignated(_)
            | CryptoShredError::EncryptionNotConfigured
            | CryptoShredError::SubjectErased(_) => "FAILED_PRECONDITION",
            CryptoShredError::Conflict(_) => "CONFLICT",
            CryptoShredError::Crypto(_)
            | CryptoShredError::Io(_)
            | CryptoShredError::KeyringCorrupt(_) => "INTERNAL",
        }
    }

    /// Whether a caller may retry the operation unchanged. Always `false` here:
    /// every crypto-shred failure is either caller-fault or a hard durability /
    /// crypto fault requiring intervention, never a transient class.
    #[must_use]
    pub fn retriable(&self) -> bool {
        false
    }
}

impl From<CryptoShredError> for crate::core::error::Error {
    fn from(e: CryptoShredError) -> Self {
        match e.code() {
            "FAILED_PRECONDITION" => crate::core::error::Error::FailedPrecondition(e.to_string()),
            _ => crate::core::error::Error::Other(e.to_string()),
        }
    }
}
