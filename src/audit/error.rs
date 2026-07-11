//! Error type for the signed audit-export subsystem (Issue #3358).

use thiserror::Error;

/// Errors from building, serializing, or verifying a signed audit export.
///
/// Verification failures are deliberately specific so an auditor can tell
/// *why* an artifact was rejected (tampered content, wrong key, truncated,
/// reordered) — completeness and integrity are distinct guarantees.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The requested entity had no retrievable history (does not exist).
    #[error("entity has no exportable history: {0}")]
    NoHistory(String),

    /// A read against the database failed while building the export.
    #[error("database read failed during export: {0}")]
    DatabaseRead(String),

    /// The signing key material was malformed (wrong length / not hex).
    #[error("invalid signing key: {0}")]
    InvalidKey(String),

    /// The public key material was malformed.
    #[error("invalid public key: {0}")]
    InvalidPublicKey(String),

    /// I/O failure reading or writing key material or an artifact.
    #[error("audit I/O error: {0}")]
    Io(String),

    /// The artifact could not be serialized or parsed as JSON.
    #[error("audit artifact (de)serialization error: {0}")]
    Serialization(String),

    /// The artifact declares a format version this build cannot verify.
    #[error("unsupported audit artifact format version {found} (expected {expected})")]
    UnsupportedFormat {
        /// Version found in the artifact.
        found: u32,
        /// Version this build understands.
        expected: u32,
    },

    /// A per-version integrity leaf hash did not match — content was altered.
    #[error("content integrity check failed: {0}")]
    ContentTampered(String),

    /// The recomputed chain root did not match the embedded root — content was
    /// altered, reordered, or truncated.
    #[error(
        "chain root mismatch: artifact content is inconsistent (tampered, reordered, or truncated)"
    )]
    ChainRootMismatch,

    /// The declared version/entity count did not match the embedded records —
    /// records were added or removed.
    #[error("completeness check failed: {0}")]
    CompletenessMismatch(String),

    /// The signature did not verify against the expected public key.
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),

    /// The embedded public key did not match the caller-supplied trusted key.
    #[error("public key mismatch: artifact is signed by a different key than the trusted one")]
    KeyMismatch,
}

/// Result alias for audit operations.
pub type AuditResult<T> = std::result::Result<T, AuditError>;
