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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use zeroize::Zeroizing;

use crate::audit::{AuditPublicKey, AuditSigningKey};
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::encryption::Cipher;
use crate::encryption::factory::{Algorithm, algorithm_from_id, create_cipher};

pub mod api;
pub mod attestation;
pub mod designation;
pub mod envelope;
pub mod error;
pub mod keyring;
pub mod subject;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod integration_tests;

pub use attestation::ErasureAttestation;
pub use designation::DesignationTarget;
pub use error::CryptoShredError;
pub use subject::{SubjectId, SubjectKey};

use keyring::{BREADCRUMB_FILENAME, SubjectKeyring};

/// Crypto-shred status of a single materialized property value (PR-1b).
///
/// Surfaced as a **side-channel** from the db read boundary alongside the
/// (possibly-unsealed) [`PropertyMap`] — it is deliberately NOT a
/// [`PropertyValue`] variant and NOT a wire-format change. A property is
/// [`ShredStatus::Plaintext`] when it carried no sealed envelope, or when its
/// subject is still active and the value was unsealed in place. It is
/// [`ShredStatus::Erased`] when the value is a sealed envelope whose subject key
/// has been destroyed — the plaintext is permanently unrecoverable and the read
/// hook leaves the opaque ciphertext in place rather than fabricating a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShredStatus {
    /// Not sealed, or unsealed in place (subject active).
    Plaintext,
    /// Sealed under a subject whose key was destroyed (unrecoverable).
    Erased {
        /// The erasure subject the value belonged to.
        subject_id: String,
    },
}

/// Per-property crypto-shred status keyed by property key (PR-1b).
///
/// Only carries entries that are **not** plain plaintext (i.e. erased sealed
/// values); an absent key means [`ShredStatus::Plaintext`]. Empty when nothing
/// on the read was sealed/erased, so a non-designated read allocates nothing.
pub type ShredStatusMap = std::collections::HashMap<String, ShredStatus>;

/// A prepared sealing context threaded onto a [`crate::api::transaction::WriteTransaction`]
/// (PR-1b write plumbing).
///
/// Built once per write transaction — only when the database has at least one
/// active designation — from the shared [`CryptoShredState`], the memoized
/// subject-wrapping cipher, and the configured AEAD algorithm. Cloning is cheap
/// (two `Arc` clones + a `Copy` enum), so attaching it to a transaction is
/// effectively free; a database with no active designations attaches `None` and
/// pays nothing.
#[derive(Clone)]
pub struct SealingContext {
    state: Arc<CryptoShredState>,
    wrap_cipher: Arc<dyn Cipher>,
    algorithm: Algorithm,
}

impl std::fmt::Debug for SealingContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealingContext")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

impl SealingContext {
    /// Construct a sealing context from its parts.
    #[must_use]
    pub(crate) fn new(
        state: Arc<CryptoShredState>,
        wrap_cipher: Arc<dyn Cipher>,
        algorithm: Algorithm,
    ) -> Self {
        Self {
            state,
            wrap_cipher,
            algorithm,
        }
    }

    /// Seal the designated property values of an entity (see
    /// [`CryptoShredState::seal_map`]). Fast path when the entity carries no
    /// designated key: the map is returned unchanged.
    ///
    /// # Errors
    /// Propagates [`CryptoShredState::seal_map`]'s error (fail-closed).
    pub(crate) fn seal_map(
        &self,
        is_node: bool,
        entity_id: u64,
        props: PropertyMap,
    ) -> Result<PropertyMap, CryptoShredError> {
        self.state.seal_map(
            is_node,
            entity_id,
            props,
            self.wrap_cipher.as_ref(),
            self.algorithm,
        )
    }
}

/// AAD domain-separation prefix binding a wrapped subject DEK to its subject id
/// and key version.
const WRAP_AAD_DOMAIN: &[u8] = b"aletheiadb-subject-wrap-dek-v1";

/// Build the AEAD AAD used when wrapping / unwrapping a subject DEK.
///
/// Binds the subject id **and** the key version, so a future key-version
/// downgrade (presenting a wrap blob under the wrong version) is caught by the
/// AEAD auth check once rotation lands. Both the wrap (in `designate`) and the
/// unwrap (in `subject_key`) sides must build this identically.
fn wrap_aad(subject_id: &str, key_version: u32) -> Vec<u8> {
    let subj = subject_id.as_bytes();
    let mut aad = Vec::with_capacity(WRAP_AAD_DOMAIN.len() + 4 + subj.len());
    aad.extend_from_slice(WRAP_AAD_DOMAIN);
    aad.extend_from_slice(&key_version.to_le_bytes());
    aad.extend_from_slice(subj);
    aad
}

/// Build the sealed-envelope **write-site context** binding a value to its
/// entity id and property key (PR-1b).
///
/// Folded into the envelope AEAD AAD (alongside subject id + key version) so a
/// sealed value cannot be relocated to a different `(entity, key)` slot within
/// the same subject without a loud AEAD auth failure. The seal (write) and
/// unseal (read) sites MUST build this identically: `entity_id` little-endian
/// followed by the raw property-key bytes.
fn seal_context(entity_id: u64, key: &str) -> Vec<u8> {
    let kb = key.as_bytes();
    let mut ctx = Vec::with_capacity(8 + kb.len());
    ctx.extend_from_slice(&entity_id.to_le_bytes());
    ctx.extend_from_slice(kb);
    ctx
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
    /// Lock-free fast-path gate (PR-1b): `true` iff at least one subject is
    /// `Active`. The write hook consults this **without** locking so a database
    /// with no active designations pays zero per-transaction cost.
    has_active: AtomicBool,
    /// Lock-free fast-path gate (PR-1b): `true` iff at least one subject exists
    /// (active OR erased). The read hook consults this without locking so a
    /// database that never used crypto-shred pays zero per-read cost.
    has_any: AtomicBool,
    /// Memoized subject-wrapping cipher (PR-1b). Built lazily on the first
    /// seal/unseal from the database's encryption config and reused for the
    /// process lifetime. Safe to cache because the subject-wrapping DEK is a
    /// deterministic `HKDF(MEK, "subject-wrap")`. On a full-MEK rotation the
    /// derived cipher changes, so [`CryptoShredState::rewrap_keyring`] (Slice 5)
    /// **invalidates this cache** (sets it back to `None`) after re-wrapping the
    /// keyring, so the next seal/unseal rebuilds it under the new MEK.
    wrap_cipher: Mutex<Option<Arc<dyn Cipher>>>,
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

        let has_active = keyring.any_active();
        let has_any = !keyring.is_empty();
        let state = Self {
            keyring: Mutex::new(keyring),
            keyring_path,
            breadcrumb_path,
            signing_key,
            has_active: AtomicBool::new(has_active),
            has_any: AtomicBool::new(has_any),
            wrap_cipher: Mutex::new(None),
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
            self.refresh_gates(&keyring);
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

    /// Recompute the lock-free fast-path gates from a locked keyring guard.
    /// Called after any mutation that can change active/any state.
    fn refresh_gates(&self, guard: &SubjectKeyring) {
        self.has_active.store(guard.any_active(), Ordering::Relaxed);
        self.has_any.store(!guard.is_empty(), Ordering::Relaxed);
    }

    /// Whether any subject is currently `Active` (lock-free write-path gate).
    #[must_use]
    pub(crate) fn has_active_designations(&self) -> bool {
        self.has_active.load(Ordering::Relaxed)
    }

    /// Whether any subject exists at all (lock-free read-path gate).
    #[must_use]
    pub(crate) fn has_any_designations(&self) -> bool {
        self.has_any.load(Ordering::Relaxed)
    }

    /// The durable keyring path, if any.
    fn keyring_path(&self) -> Option<&Path> {
        self.keyring_path.as_deref()
    }

    /// Export the durable keyring/designation-registry as CRC-wrapped sidecar
    /// bytes for folding into an `.albk` backup (Issue #3665).
    ///
    /// Returns the exact [`keyring::encode_sidecar_with_crc`] wire form — the
    /// same bytes the durable `subject_keyring.dat` holds — so the restore path
    /// can write them back verbatim and have the fail-closed loader accept
    /// them. Returns an **empty** `Vec` when the keyring has no entries (nothing
    /// to fold, so a crypto-shred-free database keeps its `.albk` non-key-bearing).
    ///
    /// Wrapped DEKs are already MEK-encrypted; still, an **erased** subject's
    /// entry carries `wrapped_key = None`, so its DEK ciphertext is physically
    /// absent from the exported bytes (crypto-shred erasure survives the fold).
    #[must_use]
    pub(crate) fn export_sidecar_bytes(&self) -> Vec<u8> {
        let guard = self.lock_keyring();
        if guard.is_empty() {
            return Vec::new();
        }
        keyring::encode_sidecar_with_crc(&guard.to_sidecar())
    }

    /// Microseconds-since-epoch when `subject_id` was erased, or `None` if the
    /// subject is unknown or still active (test/introspection).
    #[cfg(test)]
    #[must_use = "the erased-at timestamp is the observed value under test; discarding it makes the call a no-op"]
    pub(crate) fn subject_erased_at(&self, subject_id: &str) -> Option<i64> {
        self.lock_keyring()
            .get(subject_id)
            .and_then(|e| e.erased_at_micros)
    }

    /// A clone of a subject's wrapped-DEK ciphertext, if present (test-only,
    /// Issue #3665 T4). Returns `None` once the subject is erased (the wrapped
    /// blob is physically removed). The returned bytes are AEAD ciphertext
    /// (never raw key material) and are used only to assert their ABSENCE from
    /// an archive — they are never logged.
    #[cfg(test)]
    #[must_use = "the wrapped-DEK ciphertext is captured to assert its absence from an archive; discarding it makes the call a no-op"]
    pub(crate) fn subject_wrapped_dek_for_test(&self, subject_id: &str) -> Option<Vec<u8>> {
        self.lock_keyring()
            .get(subject_id)
            .and_then(|e| e.wrapped_key.as_ref())
            .map(|w| w.wrapped.clone())
    }

    /// The `key_version` stamped on a subject's wrapped DEK, or `None` if the
    /// subject is unknown or erased (test/introspection, Slice 5). Used to assert
    /// the subject-keyring re-wrap advanced the wrapping generation.
    #[cfg(test)]
    #[must_use = "the wrapped-DEK key version is the observed value under test; discarding it makes the call a no-op"]
    pub(crate) fn subject_wrapped_key_version_for_test(&self, subject_id: &str) -> Option<u32> {
        self.lock_keyring()
            .get(subject_id)
            .and_then(|e| e.wrapped_key.as_ref())
            .map(|w| w.key_version)
    }

    /// The breadcrumb path, if any.
    fn breadcrumb_path(&self) -> Option<&Path> {
        self.breadcrumb_path.as_deref()
    }

    /// Whether the subject-wrapping cipher is currently memoized (test-only,
    /// Slice 5). Used to assert [`rewrap_keyring`](Self::rewrap_keyring)
    /// invalidates the cache.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn wrap_cipher_cached_for_test(&self) -> bool {
        self.wrap_cipher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
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
            let wrap_aad = wrap_aad(subject_id.as_str(), INITIAL_SUBJECT_KEY_VERSION);
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
        self.refresh_gates(&guard);
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
        let mut guard = self.lock_keyring();
        Self::dek_from_guard(&mut guard, subject_id.as_str(), wrap_cipher)
    }

    /// Fetch a subject's unwrapped DEK from an **already-locked** keyring guard.
    ///
    /// Checks the unwrapped-DEK cache first, else unwraps the stored wrapped blob
    /// with `wrap_cipher` and caches the result. Kept `&mut guard`-based (not
    /// `&self`) so the seal/unseal hot paths, which already hold the keyring lock
    /// to consult the reverse index, never re-lock (`std::sync::Mutex` is not
    /// reentrant — re-locking would deadlock).
    ///
    /// # Errors
    /// - [`CryptoShredError::NotDesignated`] if the subject is unknown.
    /// - [`CryptoShredError::SubjectErased`] if the subject was erased (key gone).
    /// - [`CryptoShredError::Crypto`] on unwrap / AEAD failure.
    fn dek_from_guard(
        guard: &mut SubjectKeyring,
        subject_id: &str,
        wrap_cipher: &dyn crate::encryption::Cipher,
    ) -> Result<SubjectKey, CryptoShredError> {
        use keyring::SubjectState;

        if let Some(cached) = guard.cached_key(subject_id) {
            return Ok(cached.clone());
        }
        let entry = guard
            .get(subject_id)
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
        let key_version = wrapped.key_version;
        let aad = wrap_aad(subject_id, key_version);
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
        guard.cache_key(subject_id, key.clone());
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
        self.refresh_gates(&guard);

        // Rewrite the keyring durably so the wrapped blob is physically gone.
        if let Some(path) = self.keyring_path() {
            keyring::save_keyring(path, &guard)?;
        }

        // NOTE (PR-1b): the durable keyring record above IS the erasure record —
        // an erased subject's key is physically gone and its state is `Erased`,
        // fail-closed across restart. A WAL-transaction erasure *tombstone*
        // (subject id + entity/version counts + timestamp, riding the
        // `is_tombstone` machinery) plus changefeed observability is deliberately
        // DEFERRED to a #3413-aligned follow-up: emitting it needs a WAL-format
        // extension to survive crash recovery, and doing it here would force
        // `db.write(...)` — acquiring `historical`/`wal` — while this method holds
        // the keyring `Mutex`, inverting the lock order (the keyring lock must be
        // *released before* the write-path primitives are taken; see the
        // lock-order note in CLAUDE.md / seams §6). So erase does NOT call
        // `db.write` in this slice — no lock-order change is required.

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

    /// Return the memoized subject-wrapping cipher, building it once via `build`
    /// on first use (see the `wrap_cipher` field doc for the caching rationale).
    ///
    /// # Errors
    /// Propagates `build`'s error (e.g. encryption not configured).
    pub(crate) fn cached_wrap_cipher(
        &self,
        build: impl FnOnce() -> Result<Box<dyn Cipher>, CryptoShredError>,
    ) -> Result<Arc<dyn Cipher>, CryptoShredError> {
        let mut guard = self
            .wrap_cipher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let cipher: Arc<dyn Cipher> = Arc::from(build()?);
        *guard = Some(Arc::clone(&cipher));
        Ok(cipher)
    }

    /// Whether any **active** subject holds a wrapped DEK stamped at a key
    /// version older than `new_version` (subject-keyring re-wrap scoping,
    /// Slice 5).
    ///
    /// Used by the rotation driver to decide whether the subject-keyring layer
    /// is in scope for a full-MEK rotation: a database with no subjects, or one
    /// whose every subject is already at (or beyond) `new_version`, needs no
    /// re-wrap pass and records the layer `Skipped`. Erased subjects (whose
    /// wrapped key is physically gone) are never stale — there is nothing to
    /// re-wrap.
    #[must_use]
    pub(crate) fn keyring_has_stale_wrapped_entries(&self, new_version: u32) -> bool {
        use keyring::SubjectState;
        let guard = self.lock_keyring();
        guard.iter_entries().any(|e| {
            e.state == SubjectState::Active
                && e.wrapped_key
                    .as_ref()
                    .is_some_and(|w| w.key_version < new_version)
        })
    }

    /// Re-wrap every stale-generation subject DEK from `old_wrap_cipher` to
    /// `new_wrap_cipher`, stamping `new_version` (subject-keyring re-wrap on a
    /// full-MEK rotation, Slice 5).
    ///
    /// A full-MEK rotation re-derives the subject-wrapping cipher
    /// (`HKDF(MEK, "subject-wrap")`), so every per-subject wrapped DEK sealed
    /// under the OLD MEK must be re-wrapped under the NEW MEK or it becomes
    /// undecryptable once the operator drops the old key. This unwraps each DEK
    /// under `old_wrap_cipher` (never leaving `Zeroizing`) and re-wraps it under
    /// `new_wrap_cipher`, advancing only the wrapping — the random per-subject
    /// DEK, the designation, lifecycle state, attestation, and timestamps are
    /// untouched.
    ///
    /// # Erased invariant & idempotency
    /// - An **erased** subject (or any entry with `wrapped_key == None`) is
    ///   SKIPPED — its key is destroyed and must never be re-introduced.
    /// - An entry already at `key_version >= new_version` is SKIPPED, so a
    ///   crash-resumed or double-invoked pass re-wraps zero entries and is a
    ///   safe no-op.
    ///
    /// After the pass the keyring is persisted **once** (atomic, durable) and
    /// the memoized `wrap_cipher` is invalidated so the next seal/unseal rebuilds
    /// it under the new MEK. Returns the number of entries re-wrapped.
    ///
    /// # Durability (v1 save-once)
    /// The keyring is saved a single time after all entries are re-wrapped, not
    /// per entry. A crash mid-pass therefore leaves the on-disk keyring at the
    /// pre-pass state; the rotation ledger records the layer `Pending`, and the
    /// idempotent resume re-runs the whole pass under the old MEK. A per-entry
    /// durable cursor (bounding the redo window for very large subject counts)
    /// is a named follow-up.
    ///
    /// # Errors
    /// - [`CryptoShredError::Crypto`] if a DEK fails to unwrap under
    ///   `old_wrap_cipher` (e.g. a wrong/new key presented mid-pass — a loud
    ///   AEAD authentication failure, never silent wrong data) or fails to
    ///   re-wrap, or unwraps to the wrong length.
    /// - [`CryptoShredError::Io`] on durable-write failure.
    pub(crate) fn rewrap_keyring(
        &self,
        old_wrap_cipher: &dyn Cipher,
        new_wrap_cipher: &dyn Cipher,
        new_version: u32,
    ) -> Result<usize, CryptoShredError> {
        use keyring::{SubjectState, WrappedKey};

        let mut guard = self.lock_keyring();

        // Plan: the (subject_id, old_key_version, wrapped_blob) of every entry
        // that needs re-wrapping. Skip erased / keyless entries (erased
        // invariant) and entries already at/beyond the target (idempotent).
        struct RewrapItem {
            subject_id: String,
            old_key_version: u32,
            wrapped: Vec<u8>,
        }
        let mut plan: Vec<RewrapItem> = Vec::new();
        for entry in guard.iter_entries() {
            if entry.state == SubjectState::Erased {
                continue;
            }
            let Some(w) = entry.wrapped_key.as_ref() else {
                continue;
            };
            if w.key_version >= new_version {
                continue;
            }
            plan.push(RewrapItem {
                subject_id: entry.subject_id.clone(),
                old_key_version: w.key_version,
                wrapped: w.wrapped.clone(),
            });
        }

        let mut rewrapped = 0usize;
        for item in &plan {
            // Unwrap the DEK under the OLD wrapping cipher, binding the entry's
            // own current key version as AAD (mirrors `dek_from_guard`). The
            // plaintext DEK never leaves `Zeroizing`.
            let old_aad = wrap_aad(&item.subject_id, item.old_key_version);
            let raw = Zeroizing::new(
                old_wrap_cipher
                    .decrypt(&item.wrapped, &old_aad)
                    .map_err(|e| CryptoShredError::Crypto(e.to_string()))?,
            );
            if raw.len() != subject::SUBJECT_KEY_LEN {
                return Err(CryptoShredError::Crypto(
                    "unwrapped subject key has wrong length".to_string(),
                ));
            }
            // Re-wrap under the NEW cipher, binding the NEW version as AAD.
            let new_aad = wrap_aad(&item.subject_id, new_version);
            let new_blob = new_wrap_cipher
                .encrypt(&raw, &new_aad)
                .map_err(|e| CryptoShredError::Crypto(e.to_string()))?;
            guard.set_wrapped_key(
                &item.subject_id,
                WrappedKey {
                    key_version: new_version,
                    wrapped: new_blob,
                },
            );
            rewrapped += 1;
        }

        // v1 save-once: one atomic, durable write after re-wrapping all entries.
        if rewrapped > 0
            && let Some(path) = self.keyring_path()
        {
            keyring::save_keyring(path, &guard)?;
        }
        drop(guard);

        // Invalidate the memoized subject-wrapping cipher so the next seal/unseal
        // rebuilds it under the new MEK (the cached one was derived from the old
        // MEK). Done unconditionally: even a zero-re-wrap pass (idempotent resume)
        // may have run because the live MEK changed.
        {
            let mut wc = self
                .wrap_cipher
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *wc = None;
        }

        Ok(rewrapped)
    }

    /// Seal the designated property values of an entity into `SUBJ` envelopes
    /// (PR-1b write hook).
    ///
    /// For every property key that an **active** subject designates (via the
    /// reverse index), the value is replaced with a sealed-envelope
    /// [`PropertyValue::Bytes`]. Engine-reserved keys (`__aletheia_*` /
    /// `__shred_*`) are never sealed (short-circuited by
    /// [`designation::should_seal_key`]). The returned map must be routed
    /// **byte-identically** to both the current and historical tiers (seal
    /// exactly once — the envelope carries a random per-encryption nonce).
    ///
    /// # Fail-closed erase-vs-seal race
    /// If a property key is designated but **every** covering subject has been
    /// **erased** (no active subject can seal it — its DEK is destroyed), the
    /// write is **aborted** with [`CryptoShredError::WriteAfterErasure`] rather
    /// than persisting the erasable value as plaintext. This closes the race in
    /// which an in-flight write buffered before an erasure would otherwise commit
    /// afterward: sealing is impossible (the key is gone), so abort is the only
    /// fail-closed outcome. A key designated by *some* active subject (even if
    /// another covering subject is erased) still seals under the first active one.
    ///
    /// # Errors
    /// - [`CryptoShredError::WriteAfterErasure`] when a designated key is covered
    ///   only by erased subjects (fail-closed abort — see above).
    /// - [`CryptoShredError::Crypto`] on DEK unwrap / AEAD seal failure. Fail-closed:
    ///   a seal error aborts the enclosing write rather than persisting plaintext.
    pub(crate) fn seal_map(
        &self,
        is_node: bool,
        entity_id: u64,
        props: PropertyMap,
        wrap_cipher: &dyn Cipher,
        algorithm: Algorithm,
    ) -> Result<PropertyMap, CryptoShredError> {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyKey;

        let mut guard = self.lock_keyring();
        let subjects: Vec<String> = guard.subjects_for_entity(is_node, entity_id).to_vec();
        if subjects.is_empty() {
            return Ok(props);
        }

        // Plan which keys to seal (resolve interned keys to owned strings first,
        // so re-interning via the builder never nests inside a resolve guard).
        struct SealItem {
            key: PropertyKey,
            key_str: String,
            value: PropertyValue,
            subject_id: String,
            key_version: u32,
        }
        let mut plan: Vec<SealItem> = Vec::new();
        for (key, value) in props.iter() {
            // Resolve the interned key to an owned String via `resolve_with` (no
            // owned-Arc deprecation, and — critically — we never re-enter the
            // interner while a resolve guard is held: `insert_by_key` below takes
            // the already-interned key, it does not re-intern a string).
            let Some(key_str) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) else {
                continue;
            };
            if designation::is_reserved_key(&key_str) {
                continue;
            }
            // Fail-closed erase-vs-seal race: choose the first ACTIVE subject that
            // designates this key. If the key is designated only by ERASED
            // subject(s) (their DEK is destroyed), we cannot seal it — abort the
            // write rather than persist erasable plaintext.
            let mut active: Option<(String, u32)> = None;
            let mut designated_by_erased: Option<String> = None;
            for subject_id in &subjects {
                let Some(entry) = guard.get(subject_id) else {
                    continue;
                };
                if !designation::any_should_seal(&entry.designation, is_node, entity_id, &key_str) {
                    continue;
                }
                match entry.state {
                    keyring::SubjectState::Active => {
                        let key_version = entry
                            .wrapped_key
                            .as_ref()
                            .map_or(keyring::INITIAL_SUBJECT_KEY_VERSION, |w| w.key_version);
                        active = Some((subject_id.clone(), key_version));
                        break;
                    }
                    keyring::SubjectState::Erased => {
                        designated_by_erased.get_or_insert_with(|| subject_id.clone());
                    }
                }
            }
            if let Some((subject_id, key_version)) = active {
                plan.push(SealItem {
                    key: *key,
                    key_str,
                    value: value.clone(),
                    subject_id,
                    key_version,
                });
            } else if let Some(subject_id) = designated_by_erased {
                // Designated, but only by erased subject(s): sealing is
                // impossible (key gone). Fail closed — abort the commit.
                return Err(CryptoShredError::WriteAfterErasure(subject_id));
            }
        }

        if plan.is_empty() {
            return Ok(props);
        }

        let mut builder = PropertyMapBuilder::from_map(props);
        for item in plan {
            let dek = Self::dek_from_guard(&mut guard, &item.subject_id, wrap_cipher)?;
            let cipher = create_cipher(algorithm, &Zeroizing::new(*dek.expose_bytes()));
            let context = seal_context(entity_id, &item.key_str);
            let sealed = envelope::seal_property_value(
                &item.value,
                &item.subject_id,
                item.key_version,
                &context,
                cipher.as_ref(),
            )?;
            builder =
                builder.insert_by_key(item.key, PropertyValue::Bytes(std::sync::Arc::from(sealed)));
        }
        Ok(builder.build())
    }

    /// Materialize sealed property values on read (PR-1b read hook).
    ///
    /// For each `SUBJ` envelope whose subject the keyring knows: if the subject
    /// is **active**, unseal to plaintext in place; if it is **erased**, leave the
    /// opaque ciphertext untouched and record [`ShredStatus::Erased`] in the
    /// returned side-channel — never fabricated, never silently dropped. A
    /// `Bytes` value that merely *looks* like an envelope but whose subject is
    /// unknown, or that fails to unseal under an active subject (tamper /
    /// corruption), is left byte-untouched (opaque ciphertext, never plaintext).
    ///
    /// Returns the (possibly-transformed) map plus a [`ShredStatusMap`] carrying
    /// only the erased keys (empty allocation-free when nothing was sealed).
    pub(crate) fn unseal_map(
        &self,
        entity_id: u64,
        props: PropertyMap,
        wrap_cipher: &dyn Cipher,
        algorithm_hint: Algorithm,
    ) -> (PropertyMap, ShredStatusMap) {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyKey;

        let mut statuses = ShredStatusMap::new();
        let mut guard = self.lock_keyring();

        // Collect envelope-bearing keys whose subject the keyring recognizes.
        struct SealedItem {
            key: PropertyKey,
            key_str: String,
            header: envelope::EnvelopeHeader,
            bytes: std::sync::Arc<[u8]>,
        }
        let mut sealed: Vec<SealedItem> = Vec::new();
        for (key, value) in props.iter() {
            let PropertyValue::Bytes(b) = value else {
                continue;
            };
            if !envelope::is_envelope(b) {
                continue;
            }
            let Ok(header) = envelope::parse_header(b) else {
                continue;
            };
            if guard.get(&header.subject_id).is_none() {
                // Looks like an envelope but no such subject — treat as opaque
                // user bytes, never as ours.
                continue;
            }
            let Some(key_str) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) else {
                continue;
            };
            sealed.push(SealedItem {
                key: *key,
                key_str,
                header,
                bytes: std::sync::Arc::clone(b),
            });
        }

        if sealed.is_empty() {
            return (props, statuses);
        }

        let mut builder = PropertyMapBuilder::from_map(props);
        for item in sealed {
            let is_erased = guard
                .get(&item.header.subject_id)
                .is_some_and(|e| e.state == keyring::SubjectState::Erased);
            if is_erased {
                // Key destroyed: leave opaque ciphertext, report erased.
                statuses.insert(
                    item.key_str,
                    ShredStatus::Erased {
                        subject_id: item.header.subject_id,
                    },
                );
                continue;
            }
            // Active subject: unseal to plaintext. On any failure leave the
            // opaque ciphertext (never fabricate a plaintext value).
            let Ok(dek) = Self::dek_from_guard(&mut guard, &item.header.subject_id, wrap_cipher)
            else {
                continue;
            };
            let algorithm = algorithm_from_id(item.header.algorithm_id).unwrap_or(algorithm_hint);
            let cipher = create_cipher(algorithm, &Zeroizing::new(*dek.expose_bytes()));
            let context = seal_context(entity_id, &item.key_str);
            if let Ok(value) =
                envelope::unseal_property_value(&item.bytes, &context, cipher.as_ref())
            {
                builder = builder.insert_by_key(item.key, value);
            }
        }
        (builder.build(), statuses)
    }
}
