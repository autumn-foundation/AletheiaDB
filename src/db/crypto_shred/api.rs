//! [`AletheiaDB`] crypto-shred API surface (Issue #3359, slice PR-1a).
//!
//! Thin wrappers that build the subject-wrapping cipher from the database's
//! encryption config and delegate to [`super::CryptoShredState`]. No live
//! data-path integration here — that is PR-1b.

use std::sync::Arc;

use crate::core::graph::{Edge, Node};
use crate::core::property::PropertyMap;
use crate::db::AletheiaDB;
use crate::encryption::{Cipher, KeyDerivation, create_cipher};

use super::error::CryptoShredError;
use super::{
    DesignationTarget, ErasureAttestation, SealingContext, ShredStatusMap, SubjectId, SubjectKey,
};

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

    // ---- PR-1b live data-path integration -------------------------------------

    /// Build a prepared [`SealingContext`] for a write transaction, or `None`
    /// when there is nothing to seal (no active designation) — the cheap common
    /// case. Fail-closed: when active designations exist but the subject-wrapping
    /// cipher cannot be built (e.g. encryption misconfigured), returns `Err` so
    /// the write aborts rather than silently persisting plaintext.
    pub(crate) fn sealing_context(&self) -> Result<Option<SealingContext>, CryptoShredError> {
        if !self.crypto_shred.has_active_designations() {
            return Ok(None);
        }
        let cfg = self
            .encryption_config
            .as_ref()
            .ok_or(CryptoShredError::EncryptionNotConfigured)?;
        let wrap = self
            .crypto_shred
            .cached_wrap_cipher(|| self.subject_wrap_cipher())?;
        Ok(Some(SealingContext::new(
            Arc::clone(&self.crypto_shred),
            wrap,
            cfg.algorithm,
        )))
    }

    /// Materialize sealed property values on read (PR-1b read hook).
    ///
    /// Unseal active-subject envelopes to plaintext in place; leave erased ones
    /// as opaque ciphertext and report them via the returned [`ShredStatusMap`].
    /// Cheap no-op (returns the map untouched) when the database has never had
    /// any designation. Building the subject-wrapping cipher (needed only to
    /// unwrap active DEKs on a cache miss) is memoized; a failure to build it
    /// leaves values opaque rather than fabricating plaintext.
    pub(crate) fn materialize_shred(
        &self,
        entity_id: u64,
        props: PropertyMap,
    ) -> (PropertyMap, ShredStatusMap) {
        if !self.crypto_shred.has_any_designations() {
            return (props, ShredStatusMap::new());
        }
        let Some(cfg) = self.encryption_config.as_ref() else {
            return (props, ShredStatusMap::new());
        };
        let wrap = match self
            .crypto_shred
            .cached_wrap_cipher(|| self.subject_wrap_cipher())
        {
            Ok(w) => w,
            Err(_) => return (props, ShredStatusMap::new()),
        };
        self.crypto_shred
            .unseal_map(entity_id, props, wrap.as_ref(), cfg.algorithm)
    }

    /// Unseal a node's designated properties for a read boundary (PR-1b).
    #[must_use]
    pub(crate) fn unseal_node_view(&self, mut node: Node) -> Node {
        if !self.crypto_shred.has_any_designations() {
            return node;
        }
        let (props, _status) = self.materialize_shred(node.id.as_u64(), node.properties);
        node.properties = props;
        node
    }

    /// Unseal an edge's designated properties for a read boundary (PR-1b).
    #[must_use]
    pub(crate) fn unseal_edge_view(&self, mut edge: Edge) -> Edge {
        if !self.crypto_shred.has_any_designations() {
            return edge;
        }
        let (props, _status) = self.materialize_shred(edge.id.as_u64(), edge.properties);
        edge.properties = props;
        edge
    }

    /// The crypto-shred status of a node's current-state properties (PR-1b).
    ///
    /// Returns a typed [`ShredStatusMap`] keyed by property key: an entry is
    /// present only for a property whose subject key has been **destroyed**
    /// ([`super::ShredStatus::Erased`]); an absent key is
    /// [`super::ShredStatus::Plaintext`] (never sealed, or sealed under a still
    /// active subject and thus readable). The map is empty for a node with no
    /// erased designated property, and empty (allocation-free) when the database
    /// has never used crypto-shred. This is the **public Rust** surface for the
    /// erased marker — the complement to [`AletheiaDB::get_node`], which returns
    /// the (possibly opaque) values but discards the status side-channel.
    ///
    /// # Errors
    /// Propagates the underlying node read error (e.g. the node does not exist).
    pub fn node_shred_status(
        &self,
        node_id: crate::core::NodeId,
    ) -> crate::core::error::Result<ShredStatusMap> {
        // Read the RAW stored node (sealed bytes intact) directly from the
        // current tier, bypassing the `get_node` unseal hook, so the status
        // reflects the stored envelopes rather than already-unsealed plaintext.
        let node = self.current.get_node(node_id)?;
        let (_props, status) = self.materialize_shred(node.id.as_u64(), node.properties);
        Ok(status)
    }

    /// The crypto-shred status of an edge's current-state properties (PR-1b).
    ///
    /// See [`AletheiaDB::node_shred_status`] — the edge analog.
    ///
    /// # Errors
    /// Propagates the underlying edge read error (e.g. the edge does not exist).
    pub fn edge_shred_status(
        &self,
        edge_id: crate::core::EdgeId,
    ) -> crate::core::error::Result<ShredStatusMap> {
        let edge = self.current.get_edge(edge_id)?;
        let (_props, status) = self.materialize_shred(edge.id.as_u64(), edge.properties);
        Ok(status)
    }
}
