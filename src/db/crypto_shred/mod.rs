//! GDPR crypto-shred foundation (Issue #3359, slice PR-1a).
//!
//! Crypto-shred implements GDPR "right to erasure" over an append-only
//! bi-temporal store by encrypting the erasable payload under a **random
//! per-subject key** and erasing by **destroying that key** — the ciphertext may
//! remain across every tier but becomes permanently undecryptable, while the
//! record structure, temporal coordinates, and hash chain stay intact and
//! verifiable.
//!
//! ## What this slice (PR-1a) contains
//!
//! The cryptographic core **in isolation** — no live data-path integration:
//! - [`subject::SubjectId`] / [`subject::SubjectKey`] — subject identity + key.
//! - [`envelope`] — the self-describing sealed-envelope codec.
//! - [`keyring`] — durable, fail-closed per-subject key registry + breadcrumb.
//! - [`designation`] — which entities / property keys a subject seals.
//! - [`attestation::ErasureAttestation`] — signed proof of key destruction.
//! - [`CryptoShredState`] + the [`crate::db::AletheiaDB`] `designate_subject` /
//!   `erase_subject` / `subject_key` API (see [`api`]).
//!
//! ## Deferred to PR-1b and later slices
//!
//! Live seal/unseal at the write/read choke points, the erasure-tombstone write
//! transaction, chain-leaf ciphertext binding (AC4), HNSW exclusion, and the
//! CLI/MCP surfaces. This slice proves key destruction → envelope unrecoverable
//! at the unit level.
//!
//! ## Why a RANDOM DEK, not `HKDF(MEK, subject_id)`
//!
//! A key derived from the MEK stays re-derivable while the MEK lives — that is
//! access-revocation, not erasure, and fails AC1. The per-subject DEK is a
//! random independent 32-byte key; destroying its only durable (wrapped) copy is
//! irreversible even for a holder of the MEK.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::audit::{AuditPublicKey, AuditSigningKey};

pub mod api;
pub mod attestation;
pub mod designation;
pub mod envelope;
pub mod error;
pub mod keyring;
pub mod subject;

#[cfg(test)]
mod tests;

pub use attestation::ErasureAttestation;
pub use designation::DesignationTarget;
pub use error::CryptoShredError;
pub use subject::{SubjectId, SubjectKey};

use keyring::{BREADCRUMB_FILENAME, SubjectKeyring};

/// AAD domain-separation prefix binding a wrapped subject DEK to its subject id
/// and key version.
const WRAP_AAD_DOMAIN: &[u8] = b"aletheiadb-subject-wrap-dek-v1";

/// Build the AEAD AAD used when wrapping / unwrapping a subject DEK.
///
/// Binds the subject id **and** the key version, so a future key-version
/// downgrade (presenting a wrap blob under the wrong version) is caught by the
/// AEAD auth check once rotation lands. Both the wrap (in `designate`) and the
/// unwrap (in `subject_key`) sides must build this identically.
fn wrap_aad(subject_id: &SubjectId, key_version: u32) -> Vec<u8> {
    let subj = subject_id.as_bytes();
    let mut aad = Vec::with_capacity(WRAP_AAD_DOMAIN.len() + 4 + subj.len());
    aad.extend_from_slice(WRAP_AAD_DOMAIN);
    aad.extend_from_slice(&key_version.to_le_bytes());
    aad.extend_from_slice(subj);
    aad
}

/// Per-database crypto-shred state: the in-memory keyring, its durable paths,
/// and the attestation signing key.
///
/// For an ephemeral database (no index persistence) `keyring_path` is `None` and
/// the keyring is in-memory only; nothing is ever written to disk and no path is
/// unwrapped, so the ephemeral construction never panics.
pub struct CryptoShredState {
    /// The in-memory per-subject key registry.
    keyring: Mutex<SubjectKeyring>,
    /// Durable keyring path, or `None` for an ephemeral (in-memory) database.
    keyring_path: Option<PathBuf>,
    /// In-flight-erase breadcrumb path (co-located with the keyring).
    breadcrumb_path: Option<PathBuf>,
    /// Ed25519 key used to sign erasure attestations. Sourced from the audit
    /// signing-key env var when set, else freshly generated for this process.
    signing_key: AuditSigningKey,
}

impl std::fmt::Debug for CryptoShredState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoShredState")
            .field("keyring_path", &self.keyring_path)
            .field("durable", &self.keyring_path.is_some())
            .finish_non_exhaustive()
    }
}

impl CryptoShredState {
    /// Open the crypto-shred state, loading the durable keyring (fail-closed) and
    /// running breadcrumb crash-recovery.
    ///
    /// `keyring_path` is `Some(<data_root>/subject_keyring.dat)` when index
    /// persistence is enabled, else `None` (in-memory only).
    ///
    /// # Breadcrumb crash-recovery (fail-closed)
    /// If an in-flight-erase breadcrumb is present, the named subject is forced
    /// to `wrapped_key = None` + `Erased` (creating a tombstone entry if absent),
    /// the keyring is rewritten durably, and the breadcrumb is cleared. A
    /// breadcrumbed subject is treated as erased no matter the keyring state, so
    /// an interrupted erase never leaves the key half-present.
    ///
    /// # Errors
    /// [`CryptoShredError::KeyringCorrupt`] if the keyring file is present but
    /// corrupt, or [`CryptoShredError::Io`] on durable-write failure during
    /// recovery.
    pub fn open(keyring_path: Option<PathBuf>) -> Result<Self, CryptoShredError> {
        // Source the attestation signing key: prefer the audit env var (stable
        // across restarts), else generate a fresh per-process key. Attestations
        // embed their signer's public key, so verification never depends on
        // re-sourcing the same key.
        //
        // OPERATIONAL NOTE: for durable deployments a stable
        // `crate::audit::SIGNING_KEY_ENV` seed SHOULD be set. Without it each
        // process generates a fresh signing key, so attestations minted before
        // and after a restart are signed by *different* keys; cross-restart
        // verification then relies solely on each attestation's embedded public
        // key (self-consistent, but with no single stable signer identity to
        // pin out of band). A stable seed gives one durable signer identity an
        // auditor can trust across restarts.
        let signing_key = AuditSigningKey::from_env(crate::audit::SIGNING_KEY_ENV)
            .unwrap_or_else(|_| AuditSigningKey::generate());

        let breadcrumb_path = keyring_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|dir| dir.join(BREADCRUMB_FILENAME));

        let keyring = match keyring_path.as_ref() {
            Some(path) => keyring::load_keyring(path)?,
            None => SubjectKeyring::new(),
        };

        let state = Self {
            keyring: Mutex::new(keyring),
            keyring_path,
            breadcrumb_path,
            signing_key,
        };

        state.recover_from_breadcrumb()?;
        Ok(state)
    }

    /// Resume an interrupted erase from a breadcrumb, if present.
    fn recover_from_breadcrumb(&self) -> Result<(), CryptoShredError> {
        let (Some(keyring_path), Some(breadcrumb_path)) =
            (self.keyring_path.as_ref(), self.breadcrumb_path.as_ref())
        else {
            return Ok(());
        };
        let Some(subject_id) = keyring::read_breadcrumb(breadcrumb_path)? else {
            return Ok(());
        };

        let now = crate::core::temporal::time::now().wallclock();
        {
            let mut keyring = self.lock_keyring();
            keyring.force_erased(&subject_id, now);
            keyring::save_keyring(keyring_path, &keyring)?;
        }
        keyring::clear_breadcrumb(breadcrumb_path);
        Ok(())
    }

    /// Whether this state is durable (backed by a keyring file).
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.keyring_path.is_some()
    }

    /// The attestation signer's public key (for verification in tests / audit).
    #[must_use]
    pub fn attestation_public_key(&self) -> AuditPublicKey {
        self.signing_key.public_key()
    }

    /// Lock the in-memory keyring. Poisoning is treated as a fatal fail — a
    /// panic while holding the keyring lock leaves erasure state ambiguous, so we
    /// prefer to surface it rather than proceed on possibly-stale key state.
    fn lock_keyring(&self) -> std::sync::MutexGuard<'_, SubjectKeyring> {
        self.keyring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The durable keyring path, if any.
    fn keyring_path(&self) -> Option<&Path> {
        self.keyring_path.as_deref()
    }

    /// The breadcrumb path, if any.
    fn breadcrumb_path(&self) -> Option<&Path> {
        self.breadcrumb_path.as_deref()
    }

    /// Reference to the attestation signing key.
    fn signing_key(&self) -> &AuditSigningKey {
        &self.signing_key
    }

    /// Designate `subject_id` over `targets`, generating and wrapping a random
    /// DEK (for a new subject) or merging targets into an existing active
    /// subject. `wrap_cipher` is the subject-wrapping cipher derived from the
    /// MEK; the DEK is wrapped with the subject id bound as AEAD AAD.
    ///
    /// # Errors
    /// - [`CryptoShredError::SubjectErased`] if the subject was already erased.
    /// - [`CryptoShredError::Crypto`] on wrap failure.
    /// - [`CryptoShredError::Io`] on durable-write failure.
    pub(crate) fn designate(
        &self,
        subject_id: &SubjectId,
        mut targets: Vec<DesignationTarget>,
        wrap_cipher: &dyn crate::encryption::Cipher,
    ) -> Result<(), CryptoShredError> {
        use keyring::{INITIAL_SUBJECT_KEY_VERSION, SubjectEntry, SubjectState, WrappedKey};

        let now = crate::core::temporal::time::now().wallclock();
        let mut guard = self.lock_keyring();

        if let Some(existing) = guard.get(subject_id.as_str()).cloned() {
            if existing.state == SubjectState::Erased {
                return Err(CryptoShredError::SubjectErased(subject_id.to_string()));
            }
            // Merge new targets into the existing active subject (dedup).
            let mut merged = existing.designation.clone();
            for t in targets.drain(..) {
                if !merged.contains(&t) {
                    merged.push(t);
                }
            }
            let mut entry = existing.clone();
            entry.designation = merged;
            guard.upsert(entry);
        } else {
            // New subject: random DEK, wrapped under the subject-wrapping cipher.
            let dek = SubjectKey::generate();
            let wrap_aad = wrap_aad(subject_id, INITIAL_SUBJECT_KEY_VERSION);
            let wrapped = wrap_cipher
                .encrypt(dek.expose_bytes(), &wrap_aad)
                .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;
            let mut deduped: Vec<DesignationTarget> = Vec::with_capacity(targets.len());
            for t in targets.drain(..) {
                if !deduped.contains(&t) {
                    deduped.push(t);
                }
            }
            let entry = SubjectEntry {
                subject_id: subject_id.as_str().to_string(),
                wrapped_key: Some(WrappedKey {
                    key_version: INITIAL_SUBJECT_KEY_VERSION,
                    wrapped,
                }),
                designation: deduped,
                state: SubjectState::Active,
                created_at_micros: now,
                erased_at_micros: None,
                attestation: None,
            };
            guard.upsert(entry);
            guard.cache_key(subject_id.as_str(), dek);
        }

        if let Some(path) = self.keyring_path() {
            keyring::save_keyring(path, &guard)?;
        }
        Ok(())
    }

    /// Unwrap and return a subject's DEK.
    ///
    /// # Errors
    /// - [`CryptoShredError::NotDesignated`] if the subject is unknown.
    /// - [`CryptoShredError::SubjectErased`] if the subject was erased (key gone).
    /// - [`CryptoShredError::Crypto`] on unwrap / AEAD failure.
    pub(crate) fn subject_key(
        &self,
        subject_id: &SubjectId,
        wrap_cipher: &dyn crate::encryption::Cipher,
    ) -> Result<SubjectKey, CryptoShredError> {
        use keyring::SubjectState;

        let mut guard = self.lock_keyring();
        if let Some(cached) = guard.cached_key(subject_id.as_str()) {
            return Ok(cached.clone());
        }
        let entry = guard
            .get(subject_id.as_str())
            .ok_or_else(|| CryptoShredError::NotDesignated(subject_id.to_string()))?;
        if entry.state == SubjectState::Erased {
            return Err(CryptoShredError::SubjectErased(subject_id.to_string()));
        }
        let wrapped = entry
            .wrapped_key
            .as_ref()
            .ok_or_else(|| CryptoShredError::SubjectErased(subject_id.to_string()))?;
        // Zeroize every transit copy of the unwrapped DEK: the decrypt output
        // lands in a Zeroizing<Vec<u8>>, and the fixed-size array we hand to
        // SubjectKey is built inside a Zeroizing<[u8;N]> — neither is left in a
        // plain buffer that could survive on the stack/heap after use.
        let aad = wrap_aad(subject_id, wrapped.key_version);
        let raw = Zeroizing::new(
            wrap_cipher
                .decrypt(&wrapped.wrapped, &aad)
                .map_err(|e| CryptoShredError::Crypto(e.to_string()))?,
        );
        if raw.len() != subject::SUBJECT_KEY_LEN {
            return Err(CryptoShredError::Crypto(
                "unwrapped subject key has wrong length".to_string(),
            ));
        }
        let mut bytes = Zeroizing::new([0u8; subject::SUBJECT_KEY_LEN]);
        bytes.copy_from_slice(&raw);
        let key = SubjectKey::from_bytes(bytes);
        guard.cache_key(subject_id.as_str(), key.clone());
        Ok(key)
    }

    /// Erase a subject: destroy its key (physically remove the wrapped blob),
    /// zeroize any cached copy, and return a signed attestation. Multi-step and
    /// crash-consistent via a breadcrumb.
    ///
    /// Re-erase is an idempotent no-op returning the recorded attestation.
    ///
    /// # Errors
    /// - [`CryptoShredError::NotDesignated`] if the subject is unknown (AC6).
    /// - [`CryptoShredError::Io`] on durable-write failure.
    pub(crate) fn erase(
        &self,
        subject_id: &SubjectId,
    ) -> Result<ErasureAttestation, CryptoShredError> {
        use keyring::SubjectState;

        let mut guard = self.lock_keyring();
        // Clone the entry up front so the guard is free to be mutated below.
        let existing = guard
            .get(subject_id.as_str())
            .cloned()
            .ok_or_else(|| CryptoShredError::NotDesignated(subject_id.to_string()))?;

        // Idempotent re-erase: return the recorded attestation if present.
        if existing.state == SubjectState::Erased {
            if let Some(record) = existing.attestation.as_ref() {
                return ErasureAttestation::from_record(record);
            }
            // Crash-recovered tombstone with no attestation: mint one now over
            // the recorded facts so the caller still gets a signed proof.
            let entity_count = existing.designation.len() as u32;
            let ts = existing
                .erased_at_micros
                .unwrap_or_else(|| crate::core::temporal::time::now().wallclock());
            let attestation =
                ErasureAttestation::sign(self.signing_key(), subject_id.as_str(), entity_count, ts);
            let mut updated = existing.clone();
            updated.attestation = Some(attestation.to_record());
            guard.upsert(updated);
            if let Some(path) = self.keyring_path() {
                keyring::save_keyring(path, &guard)?;
            }
            return Ok(attestation);
        }

        let entity_count = existing.designation.len() as u32;
        let now = crate::core::temporal::time::now().wallclock();

        // (a) Durable breadcrumb BEFORE mutating the keyring, so an interrupted
        // erase resumes deterministically (fail-closed: a breadcrumbed subject
        // is treated as erased on restart no matter the keyring state).
        if let Some(bc) = self.breadcrumb_path() {
            keyring::write_breadcrumb(bc, subject_id.as_str())?;
        }

        // (b/c/d) Build+sign the attestation, then physically remove the wrapped
        // DEK, flip to Erased, and zeroize the cached key.
        let attestation =
            ErasureAttestation::sign(self.signing_key(), subject_id.as_str(), entity_count, now);
        guard.erase_in_memory(subject_id.as_str(), now, attestation.to_record());

        // Rewrite the keyring durably so the wrapped blob is physically gone.
        if let Some(path) = self.keyring_path() {
            keyring::save_keyring(path, &guard)?;
        }

        // PR-1b SEAM: record the erasure tombstone as a normal write transaction
        // here (subject id + entity/version counts + timestamp, no properties),
        // riding the existing is_tombstone machinery. Deferred out of PR-1a so
        // this slice stays free of live data-path integration.

        // (e) Clear the breadcrumb — erase is complete.
        if let Some(bc) = self.breadcrumb_path() {
            keyring::clear_breadcrumb(bc);
        }

        Ok(attestation)
    }

    /// Whether a subject is currently designated and active (test/introspection).
    #[must_use]
    pub fn is_active(&self, subject_id: &str) -> bool {
        self.lock_keyring()
            .get(subject_id)
            .is_some_and(|e| e.state == keyring::SubjectState::Active)
    }

    /// Whether a subject has been erased (test/introspection).
    #[must_use]
    pub fn is_erased(&self, subject_id: &str) -> bool {
        self.lock_keyring()
            .get(subject_id)
            .is_some_and(|e| e.state == keyring::SubjectState::Erased)
    }
}
