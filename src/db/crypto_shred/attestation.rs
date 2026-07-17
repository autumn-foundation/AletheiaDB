//! Signed erasure attestation (Issue #3359, slice PR-1a).
//!
//! An [`ErasureAttestation`] is the auditor-facing proof that a subject was
//! erased. It carries **only** the subject id, affected entity count, and a
//! timestamp — **never any property content** — canonicalized deterministically,
//! SHA-256'd, and signed with the audit Ed25519 key
//! ([`crate::audit::AuditSigningKey::sign_root`]). The signer's public key is
//! embedded so a third party can verify the signature independently.

use bitcode::{Decode, Encode};
use sha2::{Digest, Sha256};

use crate::audit::{AuditPublicKey, AuditSigningKey};

use super::error::CryptoShredError;

/// Domain-separation tag for the attestation canonical root.
const ATTESTATION_DOMAIN: &[u8] = b"aletheiadb-erasure-attestation-v1";

/// Deterministically canonicalize the attestation fields and SHA-256 them into a
/// 32-byte signing root.
///
/// The variable-length `subject_id` is length-prefixed (injective encoding) so
/// distinct field tuples never collide.
fn canonical_root(subject_id: &str, entity_count: u32, timestamp_micros: i64) -> [u8; 32] {
    let subj = subject_id.as_bytes();
    let mut h = Sha256::new();
    h.update(ATTESTATION_DOMAIN);
    h.update((subj.len() as u64).to_le_bytes());
    h.update(subj);
    h.update(entity_count.to_le_bytes());
    h.update(timestamp_micros.to_le_bytes());
    h.finalize().into()
}

/// A signed proof that a subject's key was destroyed.
#[derive(Debug, Clone)]
pub struct ErasureAttestation {
    /// The erased subject's identifier.
    pub subject_id: String,
    /// Number of designation targets (entities) covered by the subject.
    pub entity_count: u32,
    /// Erasure timestamp, microseconds since epoch.
    pub timestamp_micros: i64,
    /// Raw 64-byte Ed25519 signature over the canonical root.
    pub signature: [u8; 64],
    /// The signer's public key (for independent verification).
    pub signer_public_key: AuditPublicKey,
}

impl ErasureAttestation {
    /// Build and sign a fresh attestation.
    pub(crate) fn sign(
        signing_key: &AuditSigningKey,
        subject_id: &str,
        entity_count: u32,
        timestamp_micros: i64,
    ) -> Self {
        let root = canonical_root(subject_id, entity_count, timestamp_micros);
        let signature = signing_key.sign_root(&root);
        Self {
            subject_id: subject_id.to_string(),
            entity_count,
            timestamp_micros,
            signature,
            signer_public_key: signing_key.public_key(),
        }
    }

    /// Verify the embedded signature against the embedded public key.
    ///
    /// Returns `true` iff the signature is valid for this attestation's
    /// canonical root. (An auditor additionally compares `signer_public_key`
    /// against a known-good key out of band.)
    #[must_use]
    pub fn verify(&self) -> bool {
        let root = canonical_root(&self.subject_id, self.entity_count, self.timestamp_micros);
        self.signer_public_key
            .verify_root(&root, &self.signature)
            .is_ok()
    }

    /// Convert to the durable (bitcode) record form.
    #[must_use]
    pub fn to_record(&self) -> AttestationRecord {
        AttestationRecord {
            subject_id: self.subject_id.clone(),
            entity_count: self.entity_count,
            timestamp_micros: self.timestamp_micros,
            signature: self.signature.to_vec(),
            signer_public_key_hex: self.signer_public_key.to_hex(),
        }
    }

    /// Reconstruct from a durable record.
    ///
    /// # Errors
    /// [`CryptoShredError::KeyringCorrupt`] if the stored signature or public key
    /// is malformed (a corrupt keyring file).
    pub fn from_record(record: &AttestationRecord) -> Result<Self, CryptoShredError> {
        let signature: [u8; 64] = record.signature.as_slice().try_into().map_err(|_| {
            CryptoShredError::KeyringCorrupt("attestation signature is not 64 bytes".to_string())
        })?;
        let signer_public_key =
            AuditPublicKey::from_hex(&record.signer_public_key_hex).map_err(|_| {
                CryptoShredError::KeyringCorrupt(
                    "attestation signer public key is malformed".to_string(),
                )
            })?;
        Ok(Self {
            subject_id: record.subject_id.clone(),
            entity_count: record.entity_count,
            timestamp_micros: record.timestamp_micros,
            signature,
            signer_public_key,
        })
    }
}

/// Durable (bitcode) form of an [`ErasureAttestation`], stored in the keyring so
/// a re-erase returns the original attestation across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct AttestationRecord {
    /// The erased subject's identifier.
    pub subject_id: String,
    /// Number of designation targets covered.
    pub entity_count: u32,
    /// Erasure timestamp, microseconds since epoch.
    pub timestamp_micros: i64,
    /// Raw 64-byte Ed25519 signature.
    pub signature: Vec<u8>,
    /// Lowercase-hex of the signer's 32-byte public key.
    pub signer_public_key_hex: String,
}
