//! [`AletheiaDB`] crypto-shred API surface (Issue #3359, slice PR-1a).
//!
//! Thin wrappers that build the subject-wrapping cipher from the database's
//! encryption config and delegate to [`super::CryptoShredState`]. No live
//! data-path integration here — that is PR-1b.

use crate::db::AletheiaDB;
use crate::encryption::{Cipher, KeyDerivation, create_cipher};

use super::error::CryptoShredError;
use super::{DesignationTarget, ErasureAttestation, SubjectId, SubjectKey};

/// HKDF component label for the subject-wrapping DEK. Yields HKDF info string
/// `aletheiadb-subject-wrap-dek-v1` (see `KeyDerivation::derive_dek`).
const SUBJECT_WRAP_COMPONENT: &str = "subject-wrap";

impl AletheiaDB {
    /// Build the subject-wrapping cipher by re-sourcing the MEK and deriving a
    /// dedicated `subject-wrap` DEK. Requires encryption to be configured.
    ///
    /// This re-sources the MEK via the public `KeyProvider` seam and derives an
    /// independent component DEK; it never touches the `EncryptionManager`'s own
    /// private component ciphers.
    fn subject_wrap_cipher(&self) -> Result<Box<dyn Cipher>, CryptoShredError> {
        let cfg = self
            .encryption_config
            .as_ref()
            .ok_or(CryptoShredError::EncryptionNotConfigured)?;
        let provider = cfg
            .key_provider
            .build_provider()
            .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;
        let mek = provider
            .get_mek()
            .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;
        let wrap_dek = KeyDerivation::new(mek)
            .derive_dek(SUBJECT_WRAP_COMPONENT)
            .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;
        Ok(create_cipher(cfg.algorithm, &wrap_dek))
    }

    /// Designate an erasure subject over one or more targets (whole entities
    /// and/or specific property keys).
    ///
    /// A new subject gets a fresh random DEK, wrapped under the MEK-derived
    /// subject-wrapping key and recorded `Active`; adding targets to an existing
    /// active subject merges them. The keyring is persisted durably when index
    /// persistence is enabled (in-memory only otherwise).
    ///
    /// # Errors
    /// - [`CryptoShredError::EncryptionNotConfigured`] if encryption is not set up.
    /// - [`CryptoShredError::InvalidArgument`] if the subject id is invalid or
    ///   `targets` is empty.
    /// - [`CryptoShredError::SubjectErased`] if the subject was already erased.
    pub fn designate_subject(
        &self,
        subject_id: impl Into<String>,
        targets: Vec<DesignationTarget>,
    ) -> Result<(), CryptoShredError> {
        let subject_id = SubjectId::new(subject_id)?;
        if targets.is_empty() {
            return Err(CryptoShredError::InvalidArgument(
                "designation requires at least one target".to_string(),
            ));
        }
        let wrap_cipher = self.subject_wrap_cipher()?;
        self.crypto_shred
            .designate(&subject_id, targets, wrap_cipher.as_ref())
    }

    /// Erase a subject: destroy its key material (unrecoverable) and return a
    /// signed [`ErasureAttestation`]. Re-erase is an idempotent no-op returning
    /// the recorded attestation.
    ///
    /// The erasure-tombstone write transaction is deferred to PR-1b; this slice
    /// proves key destruction at the cryptographic-core level.
    ///
    /// # Errors
    /// [`CryptoShredError::NotDesignated`] (`FAILED_PRECONDITION`) if the subject
    /// was never designated (AC6).
    pub fn erase_subject(
        &self,
        subject_id: impl Into<String>,
    ) -> Result<ErasureAttestation, CryptoShredError> {
        let subject_id = SubjectId::new(subject_id)?;
        self.crypto_shred.erase(&subject_id)
    }

    /// Unwrap and return a subject's DEK (for the PR-1b seal/unseal path).
    ///
    /// # Errors
    /// - [`CryptoShredError::EncryptionNotConfigured`] if encryption is not set up.
    /// - [`CryptoShredError::NotDesignated`] if the subject is unknown.
    /// - [`CryptoShredError::SubjectErased`] if the subject was erased (key gone).
    pub fn subject_key(
        &self,
        subject_id: impl Into<String>,
    ) -> Result<SubjectKey, CryptoShredError> {
        let subject_id = SubjectId::new(subject_id)?;
        let wrap_cipher = self.subject_wrap_cipher()?;
        self.crypto_shred
            .subject_key(&subject_id, wrap_cipher.as_ref())
    }

    /// The attestation signer's public key, for verifying returned attestations.
    #[must_use]
    pub fn erasure_attestation_public_key(&self) -> crate::audit::AuditPublicKey {
        self.crypto_shred.attestation_public_key()
    }
}
