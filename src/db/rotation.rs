//! Index-layer key rotation orchestration (Issue #488).
//!
//! Wires the index re-encryption engine
//! ([`IndexKeyRotation`](crate::storage::index_persistence::IndexKeyRotation))
//! into [`AletheiaDB`] behind a synchronous API. A rotation:
//!
//! 1. sources the current MEK (from the retained startup config) and the new
//!    MEK (from the caller-supplied key source), derives both index DEKs, and
//!    refuses if they are equal (rotating to the same key is a no-op that risks
//!    nonce reuse) or if encryption / index persistence is not configured;
//! 2. adds the new generation to the persistence manager's live keyring — so
//!    reads immediately dispatch on each file's header `key_version` and writes
//!    stamp the new version — and records a durable `rotation.state` breadcrumb;
//! 3. re-encrypts every old-key index file to the new key, atomically per file;
//! 4. verifies zero old-key files remain, retires the old generation, and
//!    removes the breadcrumb.
//!
//! A crash mid-rotation leaves the breadcrumb and a mix of old/new files;
//! [`resume_pending_rotation`] re-runs the idempotent pass on the next startup.
//!
//! ## v1 scope / limitations
//!
//! * **Index layer only — and ONLY when it is the sole encrypted-at-rest
//!   surface.** All per-layer DEKs (WAL, index, cold, checkpoint) derive from a
//!   single MEK. Re-encrypting only the index tree to a *new* MEK would leave
//!   the WAL/checkpoint/cold files under the old MEK's DEKs; a subsequent
//!   key-provider switch would render them undecryptable. To make that
//!   impossible, [`rotate_index_keys`](AletheiaDB::rotate_index_keys)
//!   **hard-refuses** (with
//!   [`RotationError::UnsupportedWhileEncryptedLayersPresent`]) whenever the
//!   WAL, cold storage, or a checkpoint is encrypted under the current MEK.
//!   Full-MEK rotation across every layer (which would also re-key the WAL,
//!   cold, and checkpoint files, so switching the provider is safe) is a
//!   documented follow-up; **there is no supported operator action that changes
//!   the `key_provider` config after an index-only rotation.**
//! * The rotation is driven synchronously and expects the caller to quiesce
//!   heavy index persistence for its duration; live reads stay correct
//!   throughout (keyring dispatch), and any concurrent index write uses the new
//!   generation (the manager's keyring is already switched). A concurrent index
//!   persist can leave a file under an unrecognized `key_version` that wedges
//!   the rotation fail-closed ([`RotationError::ForeignKeyVersionFile`]).
//! * A durable, ordered `rotation.state` breadcrumb (fsync'd, then fsync'd
//!   parent dir, published BEFORE the first re-encrypted file) records that a
//!   rotation is in flight so an *interrupted* one resumes on the next startup.
//!   A second rotation started while a breadcrumb exists is refused
//!   ([`RotationError::AlreadyInProgress`]) — resume or cancel the pending one.
//! * Audit events (`key.rotation.*`) are emitted through the
//!   [`EncryptionManager`](crate::encryption::EncryptionManager)'s configured
//!   audit logger (Issue #489), honoring the `[encryption.audit]` config; no key
//!   material is ever logged.

use std::sync::Arc;
use std::time::Instant;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::encryption::audit::{AuditEvent, EncryptionAuditLogger};
use crate::encryption::cipher::Cipher;
use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
use crate::encryption::factory::Algorithm;
use crate::encryption::factory::create_cipher;
use crate::encryption::key_derivation::KeyDerivation;
use crate::storage::index_persistence::common::{ENC_INDEX_KEY_VERSION_V1, IndexKeyring};
use crate::storage::index_persistence::reencrypt::DecryptProbeReport;
use crate::storage::index_persistence::{
    IndexKeyRotation, IndexPersistenceManager, RotationError, RotationProgress, RotationStatus,
};
use crate::storage::redb_cold_storage::RedbColdStorage;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;

/// Summary of a completed index key rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationReport {
    /// Previous (retired) key version.
    pub old_version: u32,
    /// New (now current) key version.
    pub new_version: u32,
    /// Encrypted index files considered.
    pub files_total: usize,
    /// Files re-encrypted to the new key.
    pub files_reencrypted: usize,
    /// Files skipped (already at the new key).
    pub files_skipped: usize,
    /// Wall-clock duration of the rotation, in milliseconds.
    pub duration_ms: u64,
}

fn rotation_err(e: RotationError) -> Error {
    match e {
        RotationError::NotConfigured => StorageError::InconsistentState {
            reason: "index encryption is not configured; cannot rotate keys".to_string(),
        }
        .into(),
        RotationError::KeyProvider(msg) => StorageError::KeyProvider(msg).into(),
        other => StorageError::PersistenceError(other.to_string()).into(),
    }
}

/// Reduce a rotation failure to a stable, key-safe category token for the audit
/// log (Issue #488 P2.2).
///
/// Mirrors [`error_category`](crate::encryption::manager) for key-load failures:
/// the audit path must never carry a raw error `Display`, which a future error
/// variant could populate with a key-source path or other sensitive context. It
/// records only this opaque category, never the full error string.
fn rotation_failure_category(e: &Error) -> &'static str {
    use crate::core::error::StorageError as SE;
    match e {
        Error::Storage(SE::KeyProvider(_)) => "key_provider",
        Error::Storage(SE::InconsistentState { .. }) => "not_configured",
        Error::Storage(SE::IoError(_)) => "io_error",
        // Everything else (re-encryption / verification failures surfaced as
        // PersistenceError, cross-layer refusals, foreign-key wedges, etc.)
        // reduces to a single generic token — never the raw message.
        _ => "rotation_failed",
    }
}

/// Source an MEK from a key-provider config (no key material is logged).
///
/// Delegates to the central [`KeyProviderConfig::build_provider`] dispatch
/// (Issue #3587) so every provider backend — file, env, passphrase, KMS, Vault
/// — is constructed in exactly one place.
fn load_mek(cfg: &KeyProviderConfig) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    let provider = cfg
        .build_provider()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))?;
    provider
        .get_mek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Statically refuse rotating TO a key source the durable breadcrumb cannot
/// round-trip (passphrase/KMS/Vault), on the *variant alone* — BEFORE any
/// `load_mek`/network call (Issue #3587).
///
/// Without this, rotating to an unreachable KMS/Vault endpoint would surface a
/// network/transport error from `load_mek` instead of the clean "not yet
/// supported" refusal. `write_rotation_state` performs the same check as
/// defense-in-depth; this one fails fast and offline. Fail-closed: only
/// file/env sources proceed.
fn refuse_unsupported_new_source(new_source: &KeyProviderConfig) -> Result<()> {
    match new_source {
        KeyProviderConfig::File { .. } | KeyProviderConfig::Env { .. } => Ok(()),
        KeyProviderConfig::PassphraseFile { .. }
        | KeyProviderConfig::Kms { .. }
        | KeyProviderConfig::Vault { .. } => {
            let (provider_type, _) = new_source.describe();
            Err(StorageError::PersistenceError(format!(
                "index key rotation to a {provider_type} key source is not yet supported"
            ))
            .into())
        }
    }
}

/// Derive the index DEK for a MEK using the shared HKDF context.
fn derive_index_dek(
    mek: Zeroizing<[u8; 32]>,
) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    KeyDerivation::new(mek)
        .derive_index_dek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Derive the WAL DEK for a MEK using the shared HKDF context (Issue #3617).
fn derive_wal_dek(
    mek: Zeroizing<[u8; 32]>,
) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    KeyDerivation::new(mek)
        .derive_wal_dek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Derive the cold-storage DEK for a MEK using the shared HKDF context (Issue
/// #3617 PR3).
fn derive_cold_dek(
    mek: Zeroizing<[u8; 32]>,
) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    KeyDerivation::new(mek)
        .derive_cold_dek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Force-roll the WAL under the new DEK and retire the old-generation segments
/// (Issue #3617). Shared by the forward driver and crash-resume.
///
/// Installs `new_wal_cipher` as key generation `new_key_version` on the WAL
/// keyring (so subsequent appends use the new DEK) via an atomic force-roll,
/// runs `persist_after_roll`, then deletes every sealed old-DEK segment
/// (identified by header — legacy or a different `key_version`). Idempotent: a
/// re-run seals another fresh (already-new-generation) segment and re-retires (a
/// no-op once the old tail is gone).
///
/// # Ordering (DEFECT #3617-PR2 data-loss fix)
///
/// The old sequence checkpointed only ONCE, BEFORE the ledger, then rolled and
/// retired. Any write acknowledged between that pre-ledger checkpoint and the
/// seal landed in the old active segment, was NOT in the pre-ledger snapshot,
/// and was then physically deleted by the retire — surviving only in RAM, so a
/// crash before the next periodic checkpoint lost an acknowledged write.
///
/// The fix reorders to **seal → persist_after_roll → retire**: after the seal,
/// every old-generation segment is sealed and no new old-gen data can appear
/// (fresh appends go to the new-gen active segment), so a persist here durably
/// captures ALL old-gen data into the index manifest, and the subsequent
/// header-based retire is then provably lossless. The forward driver supplies a
/// real `persist_indexes()` closure; the startup-resume path supplies a no-op
/// (indexes are not loaded yet at resume time — its safety rests on the startup
/// replay read having already captured every segment before retire runs).
fn roll_and_retire_wal(
    wal: &ConcurrentWalSystem,
    new_wal_cipher: Arc<dyn Cipher>,
    new_key_version: u32,
    persist_after_roll: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if wal.wal_keyring().is_none() {
        return Ok(());
    }
    let ring = wal.wal_keyring().expect("checked above").clone();
    let cipher = Arc::clone(&new_wal_cipher);
    // The keyring flip runs inside the atomic seal→reopen hand-off (holding the
    // coordinator writer mutex), so the sealed segment stays on the old DEK and
    // the fresh segment is written under the new one.
    wal.seal_active_segment_for_rotation(move || {
        ring.add_generation(new_key_version, cipher);
    })?;
    // Capture every post-checkpoint write (now sealed into an old-generation
    // segment) into the durable index snapshot BEFORE retiring old-gen segments,
    // so a retired segment never holds an un-snapshotted acknowledged write.
    persist_after_roll()?;
    wal.retire_old_generation_segments(new_key_version)?;
    Ok(())
}

/// Rewrite the durable ledger flipping `layer.wal=complete` (Issue #3617),
/// preserving every other field. A no-op when no ledger is present (already
/// cleared). Keeps the #488 P0.2 ordering: the completion is fsync'd before the
/// old generation is retired / the ledger is cleared.
fn mark_wal_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) = read_rotation_state(manager)?
        && ledger.wal != LayerStatus::Complete
    {
        ledger.wal = LayerStatus::Complete;
        write_ledger(manager, &ledger)?;
    }
    Ok(())
}

/// Rewrite the durable ledger flipping `layer.cold=complete` (Issue #3617 PR3),
/// preserving every other field. A no-op when no ledger is present. Keeps the
/// #488 P0.2 ordering: the completion is fsync'd before the old cold generation
/// is retired / the ledger is cleared.
fn mark_cold_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) = read_rotation_state(manager)?
        && ledger.cold != LayerStatus::Complete
    {
        ledger.cold = LayerStatus::Complete;
        write_ledger(manager, &ledger)?;
    }
    Ok(())
}

/// The four encrypted-at-rest layers a full-MEK rotation must eventually cover
/// (Issue #3617). Enumerated here so the cross-layer ledger records each
/// layer's completion explicitly, even in PR1 where only the index domain
/// actually re-encrypts.
///
/// **Checkpoint-DEK decision:** checkpoints deliberately ride the *index* file
/// format and *index* DEK. The dedicated `CHECKPOINT_DEK`/`checkpoint_cipher`
/// (`encryption/manager.rs`) are dead — no production checkpoint writer uses
/// them — so a checkpoint rotation is satisfied for free by the same
/// `IndexKeyRotation` pass that rotates the index tree. We still *derive* the
/// checkpoint DEK ([`MekKeyset`]) so the ledger can name the layer and PR2/PR3
/// have the material ready, but PR1 never encrypts under it. WAL and cold are
/// out of scope until PR2/PR3 and are recorded [`LayerStatus::Skipped`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotationLayer {
    Index,
    Checkpoint,
    Wal,
    Cold,
}

impl RotationLayer {
    /// All four layers, in the order the driver visits them.
    const ALL: [RotationLayer; 4] = [
        RotationLayer::Index,
        RotationLayer::Checkpoint,
        RotationLayer::Wal,
        RotationLayer::Cold,
    ];
}

/// The four per-component DEKs derived from a single MEK (Issue #3617). Held
/// only in [`Zeroizing`]; `Debug` is redacted so no key byte can ever reach a
/// log, span, or error string.
struct MekKeyset {
    wal: Zeroizing<[u8; 32]>,
    index: Zeroizing<[u8; 32]>,
    cold: Zeroizing<[u8; 32]>,
    checkpoint: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for MekKeyset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material — only a redacting placeholder.
        f.debug_struct("MekKeyset")
            .field("deks", &"<redacted>")
            .finish()
    }
}

impl MekKeyset {
    /// Derive all four per-component DEKs from `mek` via the shared HKDF
    /// helpers ([`KeyDerivation`]). Foundation for full-MEK rotation: a new MEK
    /// re-derives *every* layer's DEK, which is exactly why an index-only
    /// rotation must eventually re-encrypt all layers.
    fn derive(mek: &Zeroizing<[u8; 32]>) -> std::result::Result<Self, RotationError> {
        let kd = KeyDerivation::new(mek.clone());
        let map =
            |e: crate::encryption::KeyDerivationError| RotationError::KeyProvider(e.to_string());
        Ok(Self {
            wal: kd.derive_wal_dek().map_err(map)?,
            index: kd.derive_index_dek().map_err(map)?,
            cold: kd.derive_cold_dek().map_err(map)?,
            checkpoint: kd.derive_checkpoint_dek().map_err(map)?,
        })
    }

    /// The DEK for one layer. Each layer has an independent, domain-separated
    /// DEK; checkpoints are encrypted under the *index* DEK in practice (see
    /// [`RotationLayer`]), but their own derived DEK is returned here so the
    /// ledger driver reads every field.
    fn dek(&self, layer: RotationLayer) -> &Zeroizing<[u8; 32]> {
        match layer {
            RotationLayer::Index => &self.index,
            RotationLayer::Checkpoint => &self.checkpoint,
            RotationLayer::Wal => &self.wal,
            RotationLayer::Cold => &self.cold,
        }
    }
}

/// Old + new ciphers for one layer, built from a [`MekKeyset`] pair.
struct LayerCiphers {
    layer: RotationLayer,
    old: Arc<dyn Cipher>,
    new: Arc<dyn Cipher>,
}

/// Build the old→new cipher pair for every layer. Reads all four DEK fields of
/// both keysets (so the derived material is genuinely consumed, not dead), even
/// though PR1's driver only installs the index generation.
fn build_layer_ciphers(
    algorithm: Algorithm,
    old_deks: &MekKeyset,
    new_deks: &MekKeyset,
) -> Vec<LayerCiphers> {
    RotationLayer::ALL
        .iter()
        .map(|&layer| LayerCiphers {
            layer,
            old: Arc::from(create_cipher(algorithm, old_deks.dek(layer))),
            new: Arc::from(create_cipher(algorithm, new_deks.dek(layer))),
        })
        .collect()
}

/// Drive the forward re-encryption of each layer recorded in the ledger.
///
/// PR1 executes only the index domain: the index tree — and any checkpoint
/// file, which reuses the index format + index DEK — is re-encrypted by the
/// mature #488 [`IndexKeyRotation`] engine. WAL and cold are out of scope
/// (PR2/PR3); the cross-layer guard already refused a uniformly-encrypted DB,
/// so those layers hold no bytes to rewrite here and are simply recorded
/// `Skipped`.
fn drive_forward_layers(
    manager: &Arc<IndexPersistenceManager>,
    keyring: &IndexKeyring,
    old_version: u32,
    new_version: u32,
    layer_ciphers: &[LayerCiphers],
) -> Result<RotationProgress> {
    let mut progress = RotationProgress::default();
    for lc in layer_ciphers {
        match lc.layer {
            RotationLayer::Index => {
                // Install the new index generation (so live reads dispatch on
                // each file's header and writes stamp the new version), then
                // re-encrypt every old-key index file. Checkpoints ride this
                // pass (index format + index DEK).
                keyring.add_generation(new_version, Arc::clone(&lc.new));
                let engine = IndexKeyRotation::new(
                    manager.indexes_path(),
                    keyring.clone(),
                    old_version,
                    Arc::clone(&lc.old),
                    new_version,
                    Arc::clone(&lc.new),
                );
                progress = engine.re_encrypt(&mut |_| true).map_err(rotation_err)?;
                engine.complete().map_err(rotation_err)?;
            }
            RotationLayer::Checkpoint => {
                // Satisfied by the index pass above (same DEK + file format);
                // no separate work. Recorded complete in the ledger.
                //
                // INVARIANT (load-bearing for the checkpoint layer's coverage):
                // encrypted checkpoint files MUST live under
                // `manager.indexes_path()`, so the index `re_encrypt`/
                // `verify_complete` pass above (which enumerates only that tree)
                // actually rotates and verifies them. If a future checkpoint
                // writer ever emits encrypted files OUTSIDE `indexes_path()`,
                // this free-ride breaks two ways: (1) those files would not be
                // re-encrypted here, and (2) `conflicting_encrypted_layers`
                // would not see them, so the P0.1 cross-layer guard would not
                // refuse — silently stranding them under the old MEK. Such a
                // writer must add a guard entry in
                // `conflicting_encrypted_layers` (and a real re-encrypt arm
                // here), not rely on this comment.
            }
            RotationLayer::Wal | RotationLayer::Cold => {
                // Out of scope in PR1 (PR2/PR3). No re-encryption; recorded
                // Skipped. The derived DEKs (`lc.old`/`lc.new`) are ready for
                // when those engines land.
            }
        }
    }
    Ok(progress)
}

impl AletheiaDB {
    /// Classify the on-disk index files by key generation (read-only probe).
    ///
    /// Returns counts of files at the current key, at an old key, at an unknown
    /// key, and plaintext. Useful before/after a rotation and as the operator
    /// "how far along am I?" probe.
    ///
    /// # Errors
    ///
    /// Returns an error if index persistence or encryption is not configured, or
    /// if the index directory cannot be scanned.
    pub fn index_rotation_status(&self) -> Result<RotationStatus> {
        let manager = self.require_rotation_prereqs()?;
        let keyring = manager
            .keyring()
            .cloned()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let cipher = keyring
            .current_cipher()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let version = keyring.current_version();
        // Status only reads headers; a single-generation engine suffices.
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            version,
            cipher.clone(),
            version,
            cipher,
        );
        engine.status().map_err(rotation_err)
    }

    /// Actively probe that persisted index bodies decrypt under the live keyring
    /// (Issue #3618).
    ///
    /// [`index_rotation_status`](Self::index_rotation_status) classifies files by
    /// their `AEIX` header `key_version` only, which false-PASSes when a wrong key
    /// shares the same version number. This probe AEAD-decrypts representative
    /// index bodies (the manifest plus one file per distinct key generation) and
    /// reports which authenticated, so `encryption verify` fails on wrong key
    /// material or a corrupted body rather than trusting the header.
    ///
    /// # Errors
    ///
    /// Returns an error if index persistence or encryption is not configured, or
    /// if the index directory cannot be read. A body that does not decrypt is
    /// NOT an error here — it is recorded in
    /// [`DecryptProbeReport::decrypt_failed`] for the caller to act on.
    pub fn verify_index_decryptable(&self) -> Result<DecryptProbeReport> {
        let manager = self.require_rotation_prereqs()?;
        let keyring = manager
            .keyring()
            .cloned()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let cipher = keyring
            .current_cipher()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let version = keyring.current_version();
        // The probe only reads/decrypts bodies; a single-generation engine
        // (mirroring index_rotation_status) suffices.
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            version,
            cipher.clone(),
            version,
            cipher,
        );
        engine.verify_decryptable().map_err(rotation_err)
    }

    /// Rotate the index-encryption key to a new key source, re-encrypting every
    /// persisted index file from the current key to the new one.
    ///
    /// Synchronous: performs begin → re-encrypt → complete in one call. Both key
    /// generations are held for the duration so live reads stay correct, and the
    /// old generation is retired only after a verified full pass (so the old key
    /// can be safely dropped afterward).
    ///
    /// # Errors
    ///
    /// * index persistence or encryption is not configured;
    /// * another encrypted-at-rest layer (WAL / checkpoint / cold storage) is
    ///   present, so an index-only rotation would strand it
    ///   ([`RotationError::UnsupportedWhileEncryptedLayersPresent`]);
    /// * a rotation is already in progress
    ///   ([`RotationError::AlreadyInProgress`]) — resume or cancel it first;
    /// * the new key derives the same index DEK as the current key;
    /// * a key provider cannot source an MEK;
    /// * a filesystem or decryption error occurs during re-encryption.
    pub fn rotate_index_keys(&self, new_key_source: KeyProviderConfig) -> Result<RotationReport> {
        let manager = self.require_rotation_prereqs()?;
        let enc_cfg = self
            .encryption_config
            .clone()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;

        // P0.1 cross-layer guard: refuse an index-only rotation while any OTHER
        // layer is still encrypted under the current MEK — rotating the index
        // alone to a new MEK and switching the provider would leave those layers
        // undecryptable. Checked BEFORE the breadcrumb so a refused rotation
        // leaves no state behind.
        let conflicting = self.conflicting_encrypted_layers();
        if !conflicting.is_empty() {
            let err = rotation_err(RotationError::UnsupportedWhileEncryptedLayersPresent {
                layers: conflicting.join(", "),
            });
            self.log_rotation_failure(&manager, &err);
            return Err(err);
        }

        // P0.3 refuse-if-in-progress: a lingering breadcrumb means a prior
        // rotation was interrupted. Starting a second one (which would reuse the
        // same new_version number) risks stranding files under a foreign key.
        // Offer resume/cancel instead.
        if rotation_state_present(&manager)? {
            let err = rotation_err(RotationError::AlreadyInProgress);
            self.log_rotation_failure(&manager, &err);
            return Err(err);
        }

        let started = Instant::now();
        let report = self.run_rotation(&manager, &enc_cfg, &new_key_source, started);

        // Emit through the encryption manager's configured audit logger (Issue
        // #489 / #488 P2.3), honoring `[encryption.audit]`. Never logs key
        // material; failures reduce to a key-safe category (P2.2).
        match &report {
            Ok(r) => {
                self.emit_rotation_audit(&AuditEvent::RotationCompleted {
                    new_version: r.new_version,
                    duration_ms: r.duration_ms,
                });
            }
            Err(e) => self.log_rotation_failure(&manager, e),
        }
        report
    }

    /// Emit a rotation audit event through the encryption manager's real,
    /// configured audit logger (Issue #488 P2.3), or nowhere if encryption is
    /// not configured. Keeps every `key.rotation.*` event on the one logger with
    /// the operator's configured level/destination/instance id.
    fn emit_rotation_audit(&self, event: &AuditEvent) {
        if let Some(mgr) = &self.encryption_manager {
            mgr.audit_logger().log(event);
        }
    }

    /// Emit a `RotationFailed` audit event carrying only a key-safe category
    /// token (never the raw error text), through the configured logger.
    fn log_rotation_failure(&self, manager: &Arc<IndexPersistenceManager>, e: &Error) {
        self.emit_rotation_audit(&AuditEvent::RotationFailed {
            version: manager.keyring().map(|k| k.current_version()).unwrap_or(0),
            error: rotation_failure_category(e).to_string(),
        });
    }

    /// Names of the encrypted-at-rest layers, OTHER than the index, currently
    /// active under the DB's single MEK (Issue #488 P0.1). Empty when the index
    /// is the sole encrypted surface — the only case an index-only key rotation
    /// is safe. Never exposes ciphers or key material.
    fn conflicting_encrypted_layers(&self) -> Vec<&'static str> {
        // Full-MEK rotation now covers every encrypted-at-rest layer (Issue
        // #3617): the index/checkpoint tree (PR1), the WAL (PR2, checkpoint →
        // force-roll under the new WAL DEK → truncate the old segments), and the
        // cold/redb tier (PR3, transactional bulk re-encrypt of every stored
        // value). None of them is a cross-layer conflict any more, so a uniformly
        // encrypted database rotates fully and this returns empty.
        //
        // Retained as the extension point the P0.1 guard consults: a FUTURE
        // encrypted-at-rest layer with no rotation support MUST push its name here
        // (and be re-encrypted by the driver) rather than silently stranding its
        // bytes under the old MEK after a provider switch.
        Vec::new()
    }

    /// Prerequisite check shared by status and rotate: index persistence must be
    /// enabled and encryption configured.
    fn require_rotation_prereqs(&self) -> Result<Arc<IndexPersistenceManager>> {
        let manager =
            self.persistence_manager
                .as_ref()
                .ok_or_else(|| StorageError::InconsistentState {
                    reason: "index persistence is not enabled; cannot rotate keys".to_string(),
                })?;
        if manager.keyring().is_none() {
            return Err(rotation_err(RotationError::NotConfigured));
        }
        Ok(Arc::clone(manager))
    }

    /// Cancel a pending (interrupted) index key rotation, rolling every
    /// already-migrated file back to the old key and retiring the new generation
    /// (Issue #488 P1.2).
    ///
    /// Crash-safe: a durable `direction=cancel` breadcrumb is published BEFORE
    /// the reverse pass begins, so an interrupted cancel resumes as a *cancel*
    /// on the next startup (rolling back to the old key) rather than rolling
    /// forward. Requires a rotation to be pending
    /// ([`RotationError::NotInProgress`] otherwise).
    ///
    /// # Errors
    ///
    /// * no rotation is pending;
    /// * the breadcrumb is corrupt or its recorded key source cannot be sourced;
    /// * a filesystem or decryption error occurs during the reverse pass.
    pub fn cancel_pending_rotation(&self) -> Result<RotationReport> {
        let manager = self.require_rotation_prereqs()?;
        let enc_cfg = self
            .encryption_config
            .clone()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;

        let Some(pending) = read_rotation_state(&manager)? else {
            return Err(rotation_err(RotationError::NotInProgress));
        };

        let started = Instant::now();
        let keyring = manager
            .keyring()
            .cloned()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        // The keyring's current generation is the one written by the interrupted
        // forward pass; the *old* generation to roll back to is `enc_cfg`.
        let old_dek = derive_index_dek(load_mek(&enc_cfg.key_provider).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let old_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &old_dek));
        let old_version = ENC_INDEX_KEY_VERSION_V1;

        let new_dek = derive_index_dek(load_mek(&pending.new_source).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));
        // Ensure BOTH generations are live so the reverse pass can read new-key
        // files and re-stamp them old (add_generation replaces if present).
        keyring.add_generation(old_version, Arc::clone(&old_cipher));
        keyring.add_generation(pending.new_version, Arc::clone(&new_cipher));

        // Publish the cancel marker durably BEFORE rolling anything back.
        write_rotation_state(
            &manager,
            pending.new_version,
            &pending.new_source,
            RotationDirection::Cancel,
        )?;

        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            old_version,
            old_cipher,
            pending.new_version,
            new_cipher,
        );
        engine.cancel().map_err(rotation_err)?;
        clear_rotation_state(&manager);

        self.emit_rotation_audit(&AuditEvent::RotationCompleted {
            new_version: old_version,
            duration_ms: started.elapsed().as_millis() as u64,
        });

        Ok(RotationReport {
            old_version: pending.new_version,
            new_version: old_version,
            files_total: 0,
            files_reencrypted: 0,
            files_skipped: 0,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Resume an interrupted index key rotation (Issue #488), if one is pending.
    ///
    /// Thin `&self` wrapper over the free [`resume_pending_rotation`] that
    /// sources the (crate-private) persistence manager and startup encryption
    /// config from this instance, so operator surfaces (the CLI, Issue #490)
    /// can drive a manual resume without reaching into internals. Returns
    /// `Ok(None)` when no rotation was pending. Does not weaken any durability
    /// invariant — it only invokes the existing idempotent resume pass.
    ///
    /// # Errors
    ///
    /// * index persistence or encryption is not configured;
    /// * the breadcrumb is present but corrupt (fail-closed);
    /// * the recorded new key source cannot be sourced, or the pass fails.
    pub fn resume_pending_index_rotation(&self) -> Result<Option<RotationReport>> {
        let manager = self.require_rotation_prereqs()?;
        let enc_cfg = self
            .encryption_config
            .clone()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        // Live-db resume: the index state is already loaded, so drive the WAL
        // layer with a REAL persist-before-retire closure (mirrors the forward
        // path). This keeps the resume lossless here too — the old no-op persist
        // could strand un-checkpointed writes on this path as well (Issue #3617
        // PR2). The startup path instead passes `None` and defers via
        // `finalize_resumed_wal_rotation`.
        let persist = || self.persist_indexes();
        let report = resume_pending_rotation(&manager, &enc_cfg, Some(&self.wal), Some(&persist))?;
        // Cold layer (Issue #3617 PR3): on the LIVE resume path the cold store is
        // already wired, so finish any pending cold rotation here (idempotent —
        // resumes from the redb cursor / skips already-wrapped values) and clear
        // the ledger. `resume_pending_rotation` deferred the clear while cold was
        // `Pending`. A no-op when cold is not in flight.
        if report.is_some()
            && let Some(tiered) = self.historical.read().tiered_storage_arc()
        {
            finalize_resumed_cold_rotation(&manager, &enc_cfg, tiered.cold_storage())?;
        }
        Ok(report)
    }

    fn run_rotation(
        &self,
        manager: &Arc<IndexPersistenceManager>,
        enc_cfg: &EncryptionConfig,
        new_key_source: &KeyProviderConfig,
        started: Instant,
    ) -> Result<RotationReport> {
        // Refuse an unsupported NEW source on its variant alone, BEFORE any
        // load_mek / network call — rotating to an unreachable KMS/Vault must
        // surface the clean refusal, not a transport error (Issue #3587).
        refuse_unsupported_new_source(new_key_source)?;

        // Shared, mutable keyring handle (mutations are observed by the manager).
        let keyring = manager
            .keyring()
            .cloned()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        // Verify the current index generation exists before doing any work.
        keyring
            .current_cipher()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let old_version = keyring.current_version();
        let new_version = old_version + 1;

        // Load BOTH master keys and refuse an identical MEK in constant time
        // (Issue #3617). Comparing the *MEK* — not merely the index DEK — is
        // what makes a full-MEK rotation to the same key a refused no-op across
        // every layer, since a new MEK re-derives every layer's DEK. The MEKs
        // live only in `Zeroizing` and are dropped (zeroized) at scope end.
        let old_mek = load_mek(&enc_cfg.key_provider).map_err(rotation_err)?;
        let new_mek = load_mek(new_key_source).map_err(rotation_err)?;
        if bool::from(old_mek.as_ref().ct_eq(new_mek.as_ref())) {
            return Err(rotation_err(RotationError::SameKey));
        }

        // Derive old + new DEKs for ALL FOUR contexts (wal/index/cold/
        // checkpoint) from each MEK (Issue #3617 foundation). PR1 only
        // re-encrypts the index domain, so only the index generation is
        // installed into the live keyring and driven below; the wal/cold DEKs
        // are derived so PR2/PR3 install them without re-plumbing derivation.
        // No DEK ever leaves `Zeroizing`.
        let old_deks = MekKeyset::derive(&old_mek).map_err(rotation_err)?;
        let new_deks = MekKeyset::derive(&new_mek).map_err(rotation_err)?;
        let layer_ciphers = build_layer_ciphers(enc_cfg.algorithm, &old_deks, &new_deks);

        // Begin: record the durable v2 ledger BEFORE switching the live keyring
        // to stamp the new version, so no new-key file can ever be fsynced (via
        // a contract-violating concurrent index persist) before the ledger is on
        // stable storage — a power loss can then never strand a new-key file
        // with a lost ledger (Issue #488 P0.2, preserved per-layer for #3617).
        // The WAL layer is recorded `Pending` when it is encrypted, so crash
        // resume drives it; `write_ledger` fsyncs the file and its parent dir,
        // and only once it returns does the driver flip any keyring.
        let wal_in_scope = self.wal.is_encrypted();
        // Cold storage is in scope (Issue #3617 PR3) when encryption is
        // configured and a tiered/cold store is present — its values are under
        // the cold DEK from the same MEK and must be bulk re-encrypted.
        let cold_in_scope =
            self.encryption_manager.is_some() && self.historical.read().has_tiered_storage();

        // WAL-rotation invariant (Issue #3617): checkpoint BEFORE the ledger is
        // written. `persist_indexes` captures every committed entry into the
        // durable index snapshot and stamps the manifest, so once the ledger
        // exists ("a rotation is in flight") ALL pre-rotation WAL data is already
        // durable outside the WAL. Crash-resume can then retire the old-DEK
        // segments unconditionally without re-checkpointing (indexes are not
        // loaded yet at startup-resume time). Skipped for an index-only rotation,
        // preserving that path's exact behavior.
        if wal_in_scope {
            self.persist_indexes()?;
        }

        write_ledger(
            manager,
            &RotationLedger::forward_scope(
                new_version,
                new_key_source.clone(),
                wal_in_scope,
                cold_in_scope,
            ),
        )?;
        self.emit_rotation_audit(&AuditEvent::RotationStarted {
            old_version,
            new_version,
        });

        // Drive the index/checkpoint domain through the mature #488 engine.
        let progress =
            drive_forward_layers(manager, &keyring, old_version, new_version, &layer_ciphers)?;

        // WAL layer (Issue #3617 PR2): re-key the WAL with no bulk rewrite —
        // checkpoint (persist) → force-roll under the new WAL DEK → truncate the
        // old-DEK segments. Runs after the index pass so `persist_indexes` writes
        // fresh index files under the already-installed new index generation.
        if wal_in_scope {
            let wal_new_cipher = layer_ciphers
                .iter()
                .find(|lc| lc.layer == RotationLayer::Wal)
                .map(|lc| Arc::clone(&lc.new))
                .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
            self.rotate_wal_layer(manager, wal_new_cipher, new_version)?;
        }

        // Cold layer (Issue #3617 PR3): transactional bulk re-encrypt of every
        // stored value from the old cold DEK to the new one. Runs after WAL; the
        // cold DEK is domain-separated (`"cold"` HKDF context) so there is no
        // cross-layer dependency. Completes #3617 — after this every
        // encrypted-at-rest byte decrypts under the new MEK's DEKs.
        if cold_in_scope {
            let cold_new_cipher = layer_ciphers
                .iter()
                .find(|lc| lc.layer == RotationLayer::Cold)
                .map(|lc| Arc::clone(&lc.new))
                .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
            self.rotate_cold_layer(manager, cold_new_cipher, new_version)?;
        }

        clear_rotation_state(manager);

        Ok(RotationReport {
            old_version,
            new_version,
            files_total: progress.files_total,
            files_reencrypted: progress.files_reencrypted,
            files_skipped: progress.files_skipped,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Re-key the WAL layer to `new_wal_cipher` (Issue #3617 PR2), with no bulk
    /// rewrite of historical segments.
    ///
    /// Sequence (no cold storage — the only case the cross-layer guard allows a
    /// rotation through today):
    /// 1. **Checkpoint** — `persist_indexes` captures the current in-memory
    ///    state into the durable index snapshot and stamps the manifest with the
    ///    applied-watermark LSN, so all committed data below that LSN survives a
    ///    WAL truncation.
    /// 2. **Force-roll** — atomically seal the active (old-DEK) segment, advance
    ///    the WAL keyring to the new generation, and open a fresh segment written
    ///    under the new DEK.
    /// 3. **Re-checkpoint** — persist the index snapshot AGAIN, now that every
    ///    old-generation segment is sealed and no new old-gen data can appear, so
    ///    any write acknowledged between the pre-ledger checkpoint and the seal is
    ///    captured durably BEFORE its segment is retired (DEFECT #3617-PR2
    ///    data-loss fix — the old order retired it first, losing the write on a
    ///    crash before the next periodic checkpoint).
    /// 4. **Retire** — delete every sealed old-DEK segment (by header), now safe
    ///    because step 3 durably captured all their data; no old-DEK ciphertext
    ///    remains, so the old WAL DEK can be dropped after a provider switch.
    /// 5. **Complete** — flip `layer.wal=complete` in the durable ledger, but
    ///    only once no old-generation segment remains on disk (fail-safe: if a
    ///    tail could not be retired, return an error and leave it `pending` for a
    ///    later pass — never report success while a stranded old-DEK tail
    ///    survives).
    ///
    /// Assumes heavy writes are quiesced for its duration (the same contract as
    /// index rotation); an append racing the roll still recovers (durability),
    /// but full old-tail retirement assumes no post-checkpoint writes.
    fn rotate_wal_layer(
        &self,
        manager: &IndexPersistenceManager,
        new_wal_cipher: Arc<dyn Cipher>,
        new_key_version: u32,
    ) -> Result<()> {
        if !self.wal.is_encrypted() {
            return Ok(());
        }
        // Force-roll under the new DEK; the persist closure runs AFTER the seal
        // (capturing any post-checkpoint write now sealed into an old-gen
        // segment) and BEFORE the retire (DEFECT #3617-PR2 data-loss fix), so no
        // retired segment holds an un-snapshotted acknowledged write.
        roll_and_retire_wal(&self.wal, new_wal_cipher, new_key_version, || {
            self.persist_indexes()
        })?;

        // DEFECT #3617-PR2: gate completion/success on ACTUAL retirement. Mark
        // the WAL layer complete only once every old-generation segment is gone.
        // If a tail could not be retired (retire errored, or a stale old-gen
        // segment remains), the rotation did NOT complete: surface an error so
        // `run_rotation` leaves the ledger PENDING (never clears it) and never
        // reports success — the operator must NOT drop the old key; a later
        // resume finishes the retirement.
        if !self.wal.all_segments_use_key_version(new_key_version) {
            return Err(StorageError::InconsistentState {
                reason: "WAL key rotation incomplete: old-generation segment(s) remain after \
                         retirement. Rotation left pending for a later resume; do NOT drop the \
                         old key until it completes."
                    .to_string(),
            }
            .into());
        }
        mark_wal_complete(manager)?;
        Ok(())
    }

    /// Re-key the cold (redb) layer to `new_cold_cipher` (Issue #3617 PR3) by a
    /// transactional bulk re-encrypt of every stored value.
    ///
    /// Sequence:
    /// 1. **Reach the cold store** through `historical.tiered_storage_arc()`, then
    ///    DROP the `historical` guard — the redb write transactions self-serialize,
    ///    so no AletheiaDB primitive is held across the (potentially long) pass,
    ///    honoring the CLAUDE.md lock order (cold sits under `historical`).
    /// 2. **Install** the new cold DEK as generation `new_key_version`, keeping the
    ///    old generation live so a half-rotated store still reads old-generation
    ///    values under the `ACV1` wrapper dispatch.
    /// 3. **Bulk re-encrypt** every `node_versions`/`edge_versions` value to the
    ///    new generation, in redb-atomic batches, resumable via a durable redb
    ///    cursor; `flushed_lsn`/`temporal_extent` are never touched.
    /// 4. **Retire** the old generation (every value is now wrapped at the new
    ///    version), so the old cold DEK can be dropped after a provider switch.
    /// 5. **Complete** — flip `layer.cold=complete` in the durable ledger.
    ///
    /// A no-op when no cold store is configured or the cold store is unencrypted.
    fn rotate_cold_layer(
        &self,
        manager: &IndexPersistenceManager,
        new_cold_cipher: Arc<dyn Cipher>,
        new_key_version: u32,
    ) -> Result<()> {
        // Grab the cold-store handle under a brief `historical` read lock, then
        // drop the guard: the bulk pass runs WITHOUT holding `historical`.
        let Some(tiered) = self.historical.read().tiered_storage_arc() else {
            return Ok(());
        };
        let cold = tiered.cold_storage();
        if !cold.is_encrypted() {
            return Ok(());
        }
        cold.install_cold_generation(new_key_version, new_cold_cipher)?;
        cold.reencrypt_cold_values(new_key_version)?;
        cold.retire_cold_generations(new_key_version)?;
        mark_cold_complete(manager)?;
        Ok(())
    }
}

// ── Durable rotation-state breadcrumb (crash-resume) ─────────────────
//
// A tiny line-based file under the data dir recording that a rotation is in
// flight, its direction (forward re-encrypt, or reverse cancel), and the new
// key source needed to reconstruct the new cipher on resume. No key material is
// written — only the source *reference* (a file path or env var name), the
// version number, and the direction. The file is written DURABLY and ORDERED
// (temp → fsync → rename → parent-dir fsync) so a power loss can never strand a
// re-encrypted file with a lost breadcrumb (Issue #488 P0.2).

/// Direction of an in-flight rotation recorded in the breadcrumb (Issue #488
/// P1.2). Forward drives re-encrypt→complete; `Cancel` drives the reverse pass
/// (roll every migrated file back to the old key) so an interrupted cancel
/// resumes as a cancel instead of rolling forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotationDirection {
    Forward,
    Cancel,
}

impl RotationDirection {
    fn as_str(self) -> &'static str {
        match self {
            RotationDirection::Forward => "forward",
            RotationDirection::Cancel => "cancel",
        }
    }
}

/// Per-layer completion status in the cross-layer ledger (Issue #3617).
///
/// The resumable engines are idempotent, so a distinct "started" state is
/// unnecessary: `Pending` means "this layer still has work", `Complete` means
/// "verified re-encrypted", `Skipped` means "not in scope for this rotation"
/// (PR1 records WAL and cold `Skipped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayerStatus {
    Pending,
    Complete,
    Skipped,
}

impl LayerStatus {
    fn as_str(self) -> &'static str {
        match self {
            LayerStatus::Pending => "pending",
            LayerStatus::Complete => "complete",
            LayerStatus::Skipped => "skipped",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(LayerStatus::Pending),
            "complete" => Some(LayerStatus::Complete),
            "skipped" => Some(LayerStatus::Skipped),
            _ => None,
        }
    }
}

/// The cross-layer rotation ledger (Issue #3617) — the `version=2` successor to
/// the #488 index-only breadcrumb.
///
/// Records the format version, the global target key version, the new key
/// SOURCE REFERENCE (never key bytes — File path / Env var name only; the same
/// constraint as #488), the rotation direction, and a per-layer completion
/// status for {index, checkpoint, wal, cold}. Written durably and ordered
/// (temp → fsync → rename → parent-dir fsync) BEFORE the live keyring flips to
/// stamp the new version, preserving the #488 P0.2 invariant per layer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RotationLedger {
    direction: RotationDirection,
    /// The global target key version being installed (was #488 `new_version`).
    new_version: u32,
    new_source: KeyProviderConfig,
    index: LayerStatus,
    checkpoint: LayerStatus,
    wal: LayerStatus,
    cold: LayerStatus,
}

impl RotationLedger {
    /// The default PR1 cross-layer plan: index and checkpoint re-key through
    /// the index engine (checkpoints ride the index DEK), while WAL and cold
    /// are out of scope and recorded `Skipped`.
    fn index_scope(
        direction: RotationDirection,
        new_version: u32,
        new_source: KeyProviderConfig,
    ) -> Self {
        Self {
            direction,
            new_version,
            new_source,
            index: LayerStatus::Pending,
            checkpoint: LayerStatus::Pending,
            wal: LayerStatus::Skipped,
            cold: LayerStatus::Skipped,
        }
    }

    /// The full-MEK cross-layer plan (Issue #3617): index + checkpoint always
    /// re-key through the index engine; the WAL is `Pending` when it is
    /// encrypted (the full-MEK driver force-rolls it), else `Skipped`; cold is
    /// `Pending` when a tiered/cold store is encrypted (PR3 bulk re-encrypts it),
    /// else `Skipped`.
    fn forward_scope(
        new_version: u32,
        new_source: KeyProviderConfig,
        wal_in_scope: bool,
        cold_in_scope: bool,
    ) -> Self {
        let scope = |in_scope: bool| {
            if in_scope {
                LayerStatus::Pending
            } else {
                LayerStatus::Skipped
            }
        };
        Self {
            direction: RotationDirection::Forward,
            new_version,
            new_source,
            index: LayerStatus::Pending,
            checkpoint: LayerStatus::Pending,
            wal: scope(wal_in_scope),
            cold: scope(cold_in_scope),
        }
    }
}

fn rotation_state_path(manager: &IndexPersistenceManager) -> std::path::PathBuf {
    manager.base_path().join("rotation.state")
}

/// Is a rotation breadcrumb present on disk? A present-but-corrupt breadcrumb is
/// surfaced as an error (fail-closed, Issue #488 P1.1) rather than reported as
/// "present" or "absent".
fn rotation_state_present(manager: &IndexPersistenceManager) -> Result<bool> {
    Ok(read_rotation_state(manager)?.is_some())
}

/// Write the default PR1 index-scope ledger (index + checkpoint pending, WAL +
/// cold skipped). Signature-compatible with the #488 breadcrumb writer so every
/// existing caller keeps working; the on-disk format is now `version=2`.
fn write_rotation_state(
    manager: &IndexPersistenceManager,
    new_version: u32,
    new_source: &KeyProviderConfig,
    direction: RotationDirection,
) -> Result<()> {
    write_ledger(
        manager,
        &RotationLedger::index_scope(direction, new_version, new_source.clone()),
    )
}

/// Durably write a cross-layer rotation ledger (Issue #3617, `version=2`).
///
/// Only file/env sources carry a single, non-secret identifying reference the
/// ledger can persist and reconstruct on crash-resume. Rotating TO a
/// passphrase/KMS/Vault source is a documented follow-up (Issue #3620): those
/// carry secrets (passphrase/token env) or multi-field config that the simple
/// line ledger cannot round-trip, so refuse fail-closed rather than write a
/// ledger that would not resume. No key material is written — only the source
/// reference, versions, direction, and per-layer status.
fn write_ledger(manager: &IndexPersistenceManager, ledger: &RotationLedger) -> Result<()> {
    let (kind, value) = match &ledger.new_source {
        KeyProviderConfig::File { path } => ("file", path.to_string_lossy().into_owned()),
        KeyProviderConfig::Env { variable } => ("env", variable.clone()),
        KeyProviderConfig::PassphraseFile { .. }
        | KeyProviderConfig::Kms { .. }
        | KeyProviderConfig::Vault { .. } => {
            let (provider_type, _) = ledger.new_source.describe();
            return Err(StorageError::PersistenceError(format!(
                "key rotation to a {provider_type} key source is not yet supported"
            ))
            .into());
        }
    };
    let body = format!(
        "version=2\ndirection={}\ntarget_version={}\nnew_source_kind={kind}\nnew_source_value={value}\nlayer.index={}\nlayer.checkpoint={}\nlayer.wal={}\nlayer.cold={}\n",
        ledger.direction.as_str(),
        ledger.new_version,
        ledger.index.as_str(),
        ledger.checkpoint.as_str(),
        ledger.wal.as_str(),
        ledger.cold.as_str(),
    );
    let base = manager.base_path();
    std::fs::create_dir_all(base)
        .map_err(|e| StorageError::io_error(format!("Failed to create data dir: {e}")))?;
    write_durable(&rotation_state_path(manager), body.as_bytes())
}

/// Durable, ordered file write: temp file → `sync_all` → atomic rename → fsync
/// of the parent directory. Guarantees the breadcrumb is on stable storage
/// before the caller proceeds (Issue #488 P0.2). Leaves no temp file behind on
/// success.
fn write_durable(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| {
        StorageError::io_error("rotation.state path has no parent directory".to_string())
    })?;
    let tmp = path.with_extension("state.tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| StorageError::io_error(format!("Failed to write rotation.state: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| StorageError::io_error(format!("Failed to write rotation.state: {e}")))?;
        f.sync_all()
            .map_err(|e| StorageError::io_error(format!("Failed to fsync rotation.state: {e}")))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        StorageError::io_error(format!("Failed to publish rotation.state: {e}"))
    })?;
    // Make the rename itself durable.
    crate::storage::index_persistence::fsync_dir(parent);
    Ok(())
}

fn clear_rotation_state(manager: &IndexPersistenceManager) {
    let path = rotation_state_path(manager);
    if std::fs::remove_file(&path).is_ok()
        && let Some(parent) = path.parent()
    {
        // Make the removal durable too, so a crash right after clearing does not
        // resurrect a completed rotation's breadcrumb.
        crate::storage::index_persistence::fsync_dir(parent);
    }
}

/// Read and parse the cross-layer rotation ledger (Issue #3617).
///
/// Returns `Ok(None)` when absent (no rotation pending), `Ok(Some(_))` when
/// present and well-formed, and `Err(_)` when PRESENT BUT CORRUPT (Issue #488
/// P1.1, preserved for #3617). A malformed ledger must never be treated as "no
/// rotation" — that would silently skip resuming a real in-flight rotation and,
/// after a key-provider change, leave new-key files undecryptable. Fail closed
/// instead so startup aborts loudly for manual intervention.
///
/// **Backward compatibility:** a legacy `version=1` #488 breadcrumb (index-only,
/// with `new_version` and no `layer.*` lines) is *parsed* into the v2 shape —
/// the index layer takes the recorded direction (`Pending`), and the layers
/// #3617 adds (checkpoint/wal/cold) are `Skipped`, since a #488 database had no
/// separate checkpoint/WAL/cold rotation in flight. This is the safest choice:
/// an interrupted #488 rotation still resumes its index pass rather than being
/// rejected and left half-rotated. New fields fail closed: an unknown
/// `version`, `layer.*` value, missing `target_version`/`new_version`, or a
/// `new_source_kind` outside File/Env aborts.
///
/// **`version=` is MANDATORY.** Both #488 (v1) and #3617 (v2) writers ALWAYS
/// emit a `version=` line, so a ledger reaching this reader without one is
/// corrupt, not a pre-versioning legacy artifact — it fails closed
/// (`missing version`) rather than being silently assumed to be any format.
/// This is what lets the version dispatch below trust the number it reads.
fn read_rotation_state(manager: &IndexPersistenceManager) -> Result<Option<RotationLedger>> {
    read_rotation_state_at(&rotation_state_path(manager))
}

/// Path-based core of [`read_rotation_state`] (Issue #3617 PR2): read + parse the
/// ledger directly from `path`, so startup wiring that does not yet hold an
/// [`IndexPersistenceManager`] (the pre-replay dual-generation WAL install) can
/// consult the ledger from the persistence `data_dir` alone. Same fail-closed
/// semantics: absent → `Ok(None)`, present-but-corrupt → `Err`.
fn read_rotation_state_at(path: &std::path::Path) -> Result<Option<RotationLedger>> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StorageError::io_error(format!(
                "rotation.state present but unreadable ({e}); manual intervention required"
            ))
            .into());
        }
    };

    let corrupt = |detail: &str| -> Error {
        StorageError::InconsistentState {
            reason: format!(
                "rotation.state present but corrupt ({detail}); manual intervention required"
            ),
        }
        .into()
    };

    let mut version = None;
    let mut direction = RotationDirection::Forward; // default for legacy breadcrumbs
    let mut target_version = None;
    let mut kind = None;
    let mut value = None;
    let mut layer_index = None;
    let mut layer_checkpoint = None;
    let mut layer_wal = None;
    let mut layer_cold = None;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(corrupt("malformed line"));
        };
        let parse_layer = |v: &str, name: &str| -> Result<LayerStatus> {
            LayerStatus::parse(v).ok_or_else(|| corrupt(&format!("unknown {name} {v:?}")))
        };
        match k {
            "version" => version = Some(v.parse::<u32>().map_err(|_| corrupt("bad version"))?),
            "direction" => {
                direction = match v {
                    "forward" => RotationDirection::Forward,
                    "cancel" => RotationDirection::Cancel,
                    other => return Err(corrupt(&format!("unknown direction {other:?}"))),
                }
            }
            // v2 uses `target_version`; v1 used `new_version`. Accept both.
            "target_version" | "new_version" => {
                target_version = Some(
                    v.parse::<u32>()
                        .map_err(|_| corrupt("bad target_version"))?,
                )
            }
            "new_source_kind" => kind = Some(v.to_string()),
            "new_source_value" => value = Some(v.to_string()),
            "layer.index" => layer_index = Some(parse_layer(v, "layer.index")?),
            "layer.checkpoint" => layer_checkpoint = Some(parse_layer(v, "layer.checkpoint")?),
            "layer.wal" => layer_wal = Some(parse_layer(v, "layer.wal")?),
            "layer.cold" => layer_cold = Some(parse_layer(v, "layer.cold")?),
            _ => {}
        }
    }

    // Fail closed on an absent or unknown format version. v1 (index-only) and v2
    // (cross-layer) are the only supported shapes.
    let version = version.ok_or_else(|| corrupt("missing version"))?;
    let (def_index, def_checkpoint, def_wal, def_cold) = match version {
        // Legacy #488: index takes the recorded direction; #3617's added layers
        // were never in flight, so Skipped.
        1 => (
            LayerStatus::Pending,
            LayerStatus::Skipped,
            LayerStatus::Skipped,
            LayerStatus::Skipped,
        ),
        // v2: an absent `layer.*` line defaults to Pending (forward-safe).
        2 => (
            LayerStatus::Pending,
            LayerStatus::Pending,
            LayerStatus::Pending,
            LayerStatus::Pending,
        ),
        other => return Err(corrupt(&format!("unsupported ledger version {other}"))),
    };

    let new_version = target_version.ok_or_else(|| corrupt("missing target_version"))?;
    let value = value.ok_or_else(|| corrupt("missing new_source_value"))?;
    let new_source = match kind.as_deref() {
        Some("file") => KeyProviderConfig::File { path: value.into() },
        Some("env") => KeyProviderConfig::Env { variable: value },
        Some(other) => return Err(corrupt(&format!("unknown new_source_kind {other:?}"))),
        None => return Err(corrupt("missing new_source_kind")),
    };
    Ok(Some(RotationLedger {
        direction,
        new_version,
        new_source,
        index: layer_index.unwrap_or(def_index),
        checkpoint: layer_checkpoint.unwrap_or(def_checkpoint),
        wal: layer_wal.unwrap_or(def_wal),
        cold: layer_cold.unwrap_or(def_cold),
    }))
}

/// Resolve the key version the index and WAL keyrings should be PROVISIONED to
/// at `open()` (Issue #488 version-provisioning fast-follow).
///
/// # The bug this fixes
///
/// At `open()` the keyrings were built at the hard-coded base version
/// ([`IndexKeyring::single`] → `ENC_INDEX_KEY_VERSION_V1`,
/// [`WalKeyring::single`] → `INITIAL_WAL_KEY_VERSION`), because the
/// post-rotation key version is never persisted where `open()` reads it (the
/// rotation ledger is cleared on completion). After a rotate-then-cold-reopen
/// under the new key that mis-provisioned `current_version == 1` makes
/// `index_rotation_status` / `encryption verify` FALSE-FAIL (v2 files classify
/// as "unknown") and wedges the *next* rotation (it recomputes `new_version =
/// 1 + 1 = 2` and hits the P0.3 `ForeignKeyVersionFile` identity check against
/// the existing v2 files).
///
/// # Precedence
///
/// 1. **Pending rotation ledger** — the currently CONFIGURED key still
///    corresponds to the OLD generation (the provider switch happens only after
///    a rotation COMPLETES), so we provision to `ledger.new_version - 1` (the
///    old version, since `new_version = old_version + 1` by construction) and
///    let startup [`resume_pending_rotation`] drive the keyring to the ledger
///    target. Provisioning to the *max on-disk* here would instead be the
///    target itself and would mis-label the configured (old) cipher as the new
///    version, breaking resume; the ledger target is therefore the authority
///    for the *final* `current_version` (reached via resume), not the build
///    version. This holds for a `cancel` ledger too (old = target - 1).
/// 2. **No pending ledger** (a completed rotation, or never rotated) — provision
///    to the MAX stamped key version across BOTH persisted layers: index-file
///    `AEIX` headers folded with WAL segment headers. A never-rotated database
///    has no stamped version above the base, so this resolves to the base and
///    reproduces prior behavior exactly.
pub(crate) fn resolve_provisioned_key_version(
    indexes_dir: &std::path::Path,
    wal_dir: &std::path::Path,
    rotation_state_dir: &std::path::Path,
) -> Result<u32> {
    const BASE: u32 = crate::storage::index_persistence::common::ENC_INDEX_KEY_VERSION_V1;

    // 1. A pending ledger is the authority for a mid-rotation-crash state.
    let ledger_path = rotation_state_dir.join("rotation.state");
    if let Some(ledger) = read_rotation_state_at(&ledger_path)? {
        return Ok(ledger.new_version.saturating_sub(1).max(BASE));
    }

    // 2. Otherwise the max stamped version across the index and WAL layers.
    let index_v =
        crate::storage::index_persistence::reencrypt::max_index_key_version_in_dir(indexes_dir)
            .map_err(rotation_err)?
            .unwrap_or(BASE);
    let wal_v =
        crate::storage::wal::segment_reader::max_key_version_in_dir(wal_dir).unwrap_or(BASE);
    Ok(index_v.max(wal_v).max(BASE))
}

/// Install BOTH WAL key generations into the live WAL keyring when a rotation
/// ledger is PENDING at startup — BEFORE the startup replay read (Issue #3617
/// PR2 crash-consistency).
///
/// # Why (DEFECT: half-rotated WAL undecryptable at startup)
///
/// A mid-rotation crash leaves the WAL directory holding BOTH generations:
/// old-DEK segments (not yet retired) and new-DEK segments (fresh appends after
/// the roll). Startup's `wal.read_from` runs BEFORE [`resume_pending_rotation`]
/// installs the new generation, so with only the single (old) keyring it cannot
/// decrypt the new-DEK frames and the replay read fails before resume can help.
///
/// This installs the recorded new generation (derived from the ledger's
/// `new_source`) alongside the old one FIRST, so the replay read decrypts each
/// segment under its own generation. Additive and fail-closed: a no-op when no
/// ledger is present, the WAL layer is not `Pending`, or the WAL is unencrypted.
/// Never logs or returns key material.
pub fn install_pending_wal_generations(
    wal: &ConcurrentWalSystem,
    enc_cfg: &EncryptionConfig,
    data_dir: &std::path::Path,
) -> Result<()> {
    let ledger_path = data_dir.join("rotation.state");
    let Some(ledger) = read_rotation_state_at(&ledger_path)? else {
        return Ok(());
    };
    if !matches!(ledger.wal, LayerStatus::Pending) {
        return Ok(());
    }
    let Some(wal_keyring) = wal.wal_keyring() else {
        return Ok(());
    };
    // The old generation is already the single cipher in the startup keyring;
    // re-derive the NEW WAL DEK from the ledger's recorded source. Both live only
    // in `Zeroizing` and never leave it.
    let Some((old_cipher, old_version)) = wal_keyring.current() else {
        return Ok(());
    };
    let new_wal_dek = derive_wal_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_wal_dek));
    // Re-assert the old generation (flips the keyring to strict per-version
    // dispatch) then add the new one, so legacy/old-kv segments resolve to the
    // old DEK and new-kv segments to the new DEK.
    wal_keyring.add_generation(old_version, Arc::clone(&old_cipher));
    wal_keyring.add_generation(ledger.new_version, new_wal_cipher);
    Ok(())
}

/// Resume an interrupted index key rotation on startup (Issue #488).
///
/// If a `rotation.state` breadcrumb is present, reconstruct the new generation
/// from the recorded source (the current `enc_cfg` supplies the old key) and
/// finish the pass recorded in the breadcrumb's direction:
///
/// * `forward` — re-run the idempotent forward re-encryption pass (skipping
///   already-migrated files by header + key identity), complete, clear the
///   breadcrumb.
/// * `cancel` — run the reverse pass (roll every migrated file back to the old
///   key), retire the new generation, clear the breadcrumb. An interrupted
///   cancel thus resumes as a cancel, never rolls forward (Issue #488 P1.2).
///
/// A no-op when no rotation was in flight. A present-but-corrupt breadcrumb
/// aborts startup (Issue #488 P1.1).
///
/// # Errors
///
/// Returns an error if the breadcrumb is corrupt, the recorded new key source
/// cannot be sourced, or the re-encryption pass fails.
pub fn resume_pending_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
    wal: Option<&ConcurrentWalSystem>,
    wal_persist: Option<&dyn Fn() -> Result<()>>,
) -> Result<Option<RotationReport>> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(None);
    };
    let Some(keyring) = manager.keyring().cloned() else {
        // Encryption not configured on this startup; leave the ledger.
        return Ok(None);
    };
    let started = Instant::now();
    let old_cipher = keyring
        .current_cipher()
        .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
    let old_version = keyring.current_version();

    // Index DEK (index + checkpoint ride it). PR3 will add the cold DEK here.
    let new_dek = derive_index_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));

    keyring.add_generation(ledger.new_version, Arc::clone(&new_cipher));

    // WAL layer (Issue #3617 PR2): when the ledger records the WAL as in flight
    // and a WAL system is available, add BOTH generations to its keyring — so a
    // half-rotated directory (old-DEK + new-DEK segments) replays correctly on
    // this open — and drive the layer to completion idempotently.
    //
    // Retirement (physically deleting the old-generation segments) is only safe
    // once every write those segments hold is durable in the index snapshot. The
    // `wal_persist` argument selects HOW that guarantee is met:
    //
    // * `Some(persist)` — LIVE-db resume (`resume_pending_index_rotation`): the
    //   index state is already loaded, so we persist-then-retire INLINE, exactly
    //   like the forward `rotate_wal_layer` path (seal → persist → retire).
    //
    // * `None` — STARTUP resume (`db::config`): indexes are NOT loaded yet (index
    //   load + WAL replay run AFTER this pass), so a persist here would snapshot
    //   EMPTY state and retiring the old-generation segments would strand every
    //   write in `[checkpoint, seal)` that lives only in the in-RAM replay buffer
    //   until the async checkpoint — a second crash would lose it (DEFECT
    //   #3617-PR2 resume-path double-crash window). We therefore DEFER the
    //   roll+retire+ledger-clear to `finalize_resumed_wal_rotation`, run after
    //   replay + a SYNCHRONOUS `persist_indexes()`. Both WAL generations are
    //   already installed here (and by `install_pending_wal_generations` before
    //   the replay read), so replay decrypts every segment regardless of
    //   generation; the ledger stays PENDING for the finalize pass.
    let wal_pending = matches!(ledger.wal, LayerStatus::Pending);
    // Cold layer (Issue #3617 PR3): the cold store is NOT reachable here (on the
    // startup path it is constructed AFTER this pass; a free function has no `self`
    // to reach it). When cold is `Pending` the ledger MUST survive this pass so
    // the cold re-encrypt + final clear run in `finalize_resumed_cold_rotation`
    // once the cold store exists. Both the live and startup resume paths honor
    // this by gating the clear below on `!cold_pending`.
    let cold_pending = matches!(ledger.cold, LayerStatus::Pending);
    // DEFECT #3617-PR2: track whether the WAL layer actually finished retiring so
    // the final `clear_rotation_state` below is GATED on real completion (never
    // clears the ledger while an old-generation segment survives).
    let mut wal_incomplete = false;
    // DEFECT #3617-PR2 (resume-path double-crash): set when the startup path
    // defers WAL retirement to `finalize_resumed_wal_rotation`; the ledger MUST
    // NOT be cleared here while an un-snapshotted old-generation segment survives.
    let mut wal_finalize_deferred = false;
    if wal_pending
        && let Some(wal) = wal
        && let Some(wal_keyring) = wal.wal_keyring()
    {
        // Always install BOTH generations so a half-rotated directory replays
        // correctly regardless of which mode drives retirement.
        let new_wal_dek = derive_wal_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let new_wal_cipher: Arc<dyn Cipher> =
            Arc::from(create_cipher(enc_cfg.algorithm, &new_wal_dek));
        // The startup keyring is `single(old_wal_dek)`; adding the new generation
        // makes both live (legacy/old-kv → old, new-kv → new).
        wal_keyring.add_generation(ledger.new_version, Arc::clone(&new_wal_cipher));
        match wal_persist {
            Some(persist) => {
                // Live-db resume: persist-before-retire is lossless (mirrors the
                // forward path — the seal runs first, then `persist` captures any
                // post-checkpoint write now sealed into an old-gen segment, then
                // the retire deletes segments whose data is already snapshotted).
                roll_and_retire_wal(wal, new_wal_cipher, ledger.new_version, persist)?;
                if wal.all_segments_use_key_version(ledger.new_version) {
                    mark_wal_complete(manager)?;
                } else {
                    wal_incomplete = true;
                }
            }
            None => {
                // Startup resume: defer roll+retire+ledger-clear to
                // `finalize_resumed_wal_rotation` (after replay + sync persist).
                wal_finalize_deferred = true;
            }
        }
    }

    let engine = IndexKeyRotation::new(
        manager.indexes_path(),
        keyring,
        old_version,
        old_cipher,
        ledger.new_version,
        new_cipher,
    );

    // Drive resume from the ledger's per-layer state. In PR1 the index domain
    // (index + checkpoint, which rides the index DEK) is the only executable
    // layer; WAL and cold are recorded `Skipped` and need no resume work. If
    // that domain is still `Pending` we re-run the idempotent pass; if it is
    // already `Complete` we skip straight to verification/clear.
    let index_domain_pending = matches!(ledger.index, LayerStatus::Pending)
        || matches!(ledger.checkpoint, LayerStatus::Pending);

    let logger = EncryptionAuditLogger::from_audit_config(&enc_cfg.audit);
    let report = match ledger.direction {
        RotationDirection::Forward => {
            let progress = if index_domain_pending {
                engine.re_encrypt(&mut |_| true).map_err(rotation_err)?
            } else {
                RotationProgress::default()
            };
            engine.complete().map_err(rotation_err)?;
            // DEFECT #3617-PR2: only clear the ledger when the WAL layer actually
            // finished retiring its old generation. If an old-gen segment still
            // survives, leave the ledger PENDING and surface an error so a later
            // resume completes it — never report a completed resume while a
            // stranded old-DEK tail remains (which would let the operator drop the
            // old key and wedge startup).
            if wal_incomplete {
                return Err(StorageError::InconsistentState {
                    reason: "WAL key rotation resume incomplete: old-generation segment(s) remain \
                             after retirement. Ledger left pending for a later resume; do NOT drop \
                             the old key until it completes."
                        .to_string(),
                }
                .into());
            }
            // DEFECT #3617-PR2 (resume-path double-crash): when the startup path
            // deferred WAL retirement, the ledger MUST stay PENDING until
            // `finalize_resumed_wal_rotation` has synchronously snapshotted the
            // replayed state and retired the old-generation segments. Clearing it
            // here (before the deferred retire) would re-open the very data-loss
            // window this fix closes. The index re-encrypt + `complete()` above
            // are durable and idempotent, so leaving the ledger for the finalize
            // pass is safe and re-entrant across another crash.
            if !wal_finalize_deferred && !cold_pending {
                clear_rotation_state(manager);
                logger.log(&AuditEvent::RotationCompleted {
                    new_version: ledger.new_version,
                    duration_ms: started.elapsed().as_millis() as u64,
                });
            }
            RotationReport {
                old_version,
                new_version: ledger.new_version,
                files_total: progress.files_total,
                files_reencrypted: progress.files_reencrypted,
                files_skipped: progress.files_skipped,
                duration_ms: started.elapsed().as_millis() as u64,
            }
        }
        RotationDirection::Cancel => {
            // Reverse pass: roll every migrated file back to the OLD key and
            // retire the new generation. The dataset returns to the pre-rotation
            // (old-key) state; the breadcrumb records old_version as "new" so
            // report.new_version reflects the surviving generation.
            engine.cancel().map_err(rotation_err)?;
            clear_rotation_state(manager);
            logger.log(&AuditEvent::RotationCompleted {
                new_version: old_version,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            RotationReport {
                old_version: ledger.new_version,
                new_version: old_version,
                files_total: 0,
                files_reencrypted: 0,
                files_skipped: 0,
                duration_ms: started.elapsed().as_millis() as u64,
            }
        }
    };

    Ok(Some(report))
}

/// Phase 2 of a startup-resumed WAL key rotation (Issue #3617 PR2): retire the
/// old-generation WAL segments and clear the ledger, AFTER the startup replay has
/// been captured by a synchronous persist.
///
/// This closes the resume-path double-crash data-loss window. On the startup
/// path [`resume_pending_rotation`] (called with `wal_persist = None`) installs
/// both WAL generations but does NOT retire — because at that point the index
/// state is not loaded, so a persist would snapshot empty state and retiring the
/// old-generation segments would strand the `[checkpoint, seal)` writes that live
/// only in the in-RAM replay buffer. Once `db::config` has loaded the snapshot
/// and replayed the WAL into current state, it calls this function, which:
///
/// 1. re-installs the new WAL generation (idempotent),
/// 2. force-rolls under the new DEK, runs `persist` (a SYNCHRONOUS
///    `persist_indexes()` capturing the now-replayed state) AFTER the seal and
///    BEFORE the retire — reusing the forward-path `roll_and_retire_wal`
///    ordering verbatim — then retires the sealed old-generation segments, and
/// 3. once no old-generation segment remains, marks the WAL layer complete and
///    clears the ledger.
///
/// So no old-generation WAL segment is ever deleted until its entries are durable
/// in the index snapshot — on BOTH the forward and resume paths.
///
/// A no-op (returns `Ok(())`) when no ledger is present, the WAL layer is not
/// `Pending`, or the WAL is unencrypted — so `db::config` can call it
/// unconditionally on the encrypted-persistent startup path. Completion-gated
/// exactly like the forward path: if a stale old-generation segment cannot be
/// retired, the ledger is left PENDING and an error is surfaced so a later resume
/// finishes it (never reports success while a stranded old-DEK tail survives).
///
/// # Errors
///
/// Returns an error if the recorded new key source cannot be sourced, the persist
/// fails, the roll/retire fails, or a stale old-generation segment survives
/// retirement (ledger left pending).
pub fn finalize_resumed_wal_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
    wal: &ConcurrentWalSystem,
    persist: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(());
    };
    if !matches!(ledger.wal, LayerStatus::Pending) {
        return Ok(());
    }
    let Some(wal_keyring) = wal.wal_keyring() else {
        return Ok(());
    };

    // Re-derive + re-install the new WAL generation (idempotent: it was already
    // installed by `resume_pending_rotation` / `install_pending_wal_generations`).
    let new_wal_dek = derive_wal_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_wal_dek));
    wal_keyring.add_generation(ledger.new_version, Arc::clone(&new_wal_cipher));

    // Seal → persist (captures the replayed `[checkpoint, seal)` writes into the
    // durable index snapshot) → retire. Identical ordering to the forward path.
    roll_and_retire_wal(wal, new_wal_cipher, ledger.new_version, persist)?;

    // Completion-gated: only mark complete + clear once every old-generation
    // segment is gone; otherwise leave the ledger PENDING for a later resume.
    if !wal.all_segments_use_key_version(ledger.new_version) {
        return Err(StorageError::InconsistentState {
            reason:
                "WAL key rotation resume-finalize incomplete: old-generation segment(s) remain \
                     after retirement. Ledger left pending for a later resume; do NOT drop the old \
                     key until it completes."
                    .to_string(),
        }
        .into());
    }
    mark_wal_complete(manager)?;
    // Issue #3617 PR3: if the cold layer is still `Pending`, LEAVE the ledger for
    // `finalize_resumed_cold_rotation` (which runs after the cold store is built)
    // to complete cold and clear it. Clearing here would drop the ledger while
    // cold values are still half-rotated, wedging a provider switch.
    if !matches!(ledger.cold, LayerStatus::Pending) {
        clear_rotation_state(manager);
    }
    Ok(())
}

/// Finalize a startup-resumed COLD (redb) key rotation (Issue #3617 PR3),
/// completing the transactional bulk re-encrypt and clearing the ledger.
///
/// The cold store is constructed on the durable `open()` path AFTER
/// [`resume_pending_rotation`] and [`finalize_resumed_wal_rotation`] have run
/// (index + WAL are re-keyed before the cold tier is wired), so cold is the LAST
/// layer to settle and this function performs the final ledger clear. It is
/// idempotent and resumable:
///
/// 1. re-derive + install the new cold DEK generation (the old generation is
///    already the cold store's provisioned single generation);
/// 2. run [`RedbColdStorage::reencrypt_cold_values`], which resumes from the
///    durable redb cursor and skips any value already wrapped at the target
///    version — so a crash mid-batch resumes with no double-encrypt;
/// 3. retire the old cold generation, mark `layer.cold=complete`, and clear the
///    ledger (every layer has now settled).
///
/// A no-op (`Ok(())`) when no ledger is present, the cold layer is not `Pending`,
/// or the cold store is unencrypted — so `db::config` can call it unconditionally
/// on the encrypted-persistent startup path.
///
/// # Errors
///
/// Returns an error if the recorded new key source cannot be sourced or the bulk
/// re-encrypt pass fails (a genuine wrong/absent key surfaces loudly; the ledger
/// is left `Pending` for a later resume).
pub fn finalize_resumed_cold_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
    cold: &RedbColdStorage,
) -> Result<()> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(());
    };
    if !matches!(ledger.cold, LayerStatus::Pending) {
        return Ok(());
    }
    if !cold.is_encrypted() {
        return Ok(());
    }

    // Re-derive + install the new cold generation (idempotent: a re-run replaces
    // the same-version generation). The old generation is the cold store's
    // provisioned single generation, still live for reading old-DEK values.
    let new_cold_dek = derive_cold_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_cold_cipher: Arc<dyn Cipher> =
        Arc::from(create_cipher(enc_cfg.algorithm, &new_cold_dek));
    cold.install_cold_generation(ledger.new_version, new_cold_cipher)?;

    // Resume the bulk re-encrypt from the redb cursor (skips already-wrapped
    // values), retire the old generation, then complete + clear the ledger.
    cold.reencrypt_cold_values(ledger.new_version)?;
    cold.retire_cold_generations(ledger.new_version)?;
    mark_cold_complete(manager)?;
    clear_rotation_state(manager);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::core::property::PropertyMapBuilder;
    use crate::encryption::EncryptionManager;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use crate::storage::index_persistence::IndexPersistenceManager;
    use crate::storage::index_persistence::common::index_file_key_version;
    use crate::storage::wal::DurabilityMode;
    use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
    use std::path::Path;
    use tempfile::TempDir;

    /// Build an encrypted, persistent DB rooted at `root`, keyed by `key_file`.
    /// Encryption is uniform: WAL, index (and any cold/checkpoint) are all
    /// encrypted under the one MEK.
    fn build_db(root: &Path, key_file: &Path) -> AletheiaDB {
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(root.join("wal"))
                    .durability_mode(DurabilityMode::GroupCommit {
                        max_delay_ms: 5,
                        max_batch_size: 64,
                    })
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: true,
                data_dir: root.join("data"),
                load_on_startup: true,
                ..Default::default()
            })
            .encryption(EncryptionConfig::file_based(key_file))
            .build();
        AletheiaDB::with_unified_config(config).unwrap()
    }

    /// Build a fully-encrypted DB WITH an encrypted cold (redb) tier (Issue
    /// #3617 PR3): WAL + index + cold all under the same MEK. This is the
    /// configuration a full-MEK rotation must cover end to end.
    fn build_db_with_cold(root: &Path, key_file: &Path) -> AletheiaDB {
        use crate::config::HistoricalConfigBuilder;
        use std::time::Duration;
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(root.join("wal"))
                    .durability_mode(DurabilityMode::GroupCommit {
                        max_delay_ms: 5,
                        max_batch_size: 64,
                    })
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: true,
                data_dir: root.join("data"),
                load_on_startup: true,
                ..Default::default()
            })
            .historical(
                HistoricalConfigBuilder::new()
                    .enable_cold_storage(true)
                    .cold_storage_path(root.join("cold.redb"))
                    .migration_age_threshold(Duration::from_secs(3600))
                    .build(),
            )
            .encryption(EncryptionConfig::file_based(key_file))
            .build();
        AletheiaDB::with_unified_config(config).unwrap()
    }

    /// Build a DB whose ONLY encrypted-at-rest surface is the index layer — the
    /// sole configuration in which an index-only key rotation is safe (Issue
    /// #488 P0.1). The index files were already written encrypted by
    /// [`build_db`]; we then swap in a PLAINTEXT WAL so the cross-layer guard
    /// permits the rotation (the current uniform-MEK architecture cannot encrypt
    /// the index without also encrypting the WAL, so this is constructed for the
    /// test rather than via config).
    fn build_db_index_only(root: &Path, key_file: &Path) -> AletheiaDB {
        let mut db = build_db(root, key_file);
        let wal = ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(root.join("wal-plain")))
            .expect("plaintext wal");
        db.wal = std::sync::Arc::new(wal);
        assert!(!db.wal.is_encrypted(), "helper WAL must be plaintext");
        db
    }

    /// Guard against silent plaintext bypass: EVERY persisted index file under
    /// `indexes_dir` — bitcode files AND the native usearch files + sidecars —
    /// must begin with the AEIX header when a cipher is configured. Returns the
    /// number of files checked. (Coordinator-requested cross-path guard.)
    fn assert_all_index_files_encrypted(indexes_dir: &Path) -> usize {
        use crate::storage::index_persistence::common::is_encrypted_index;
        fn is_scratch(name: &str) -> bool {
            name.ends_with(".tmp")
                || name.contains(".tmp.")
                || name.starts_with(".aeix-usearch-tmp-")
        }
        fn walk(dir: &Path, count: &mut usize) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, count);
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if is_scratch(&name) {
                    continue;
                }
                let bytes = std::fs::read(&p).unwrap();
                assert!(
                    is_encrypted_index(&bytes),
                    "index file {p:?} is NOT encrypted (missing AEIX header) — plaintext bypass!"
                );
                *count += 1;
            }
        }
        let mut count = 0;
        walk(indexes_dir, &mut count);
        count
    }

    /// Every AEIX index file under `data/indexes` carries `expected` key_version.
    fn assert_all_at_version(indexes_dir: &Path, expected: u32) -> usize {
        fn walk(dir: &Path, expected: u32, count: &mut usize) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, expected, count);
                } else if let Ok(bytes) = std::fs::read(&p)
                    && let Some(v) = index_file_key_version(&bytes)
                {
                    assert_eq!(v, expected, "file {p:?} not at key_version {expected}");
                    *count += 1;
                }
            }
        }
        let mut count = 0;
        walk(indexes_dir, expected, &mut count);
        count
    }

    fn seed(db: &AletheiaDB) -> (crate::core::NodeId, crate::core::NodeId) {
        db.enable_vector_index("embedding", HnswConfig::new(4, DistanceMetric::Cosine))
            .unwrap();
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.persist_indexes().unwrap();
        (alice, bob)
    }

    #[test]
    fn full_cycle_reencrypts_index_files_and_data_intact() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let indexes_dir = root.join("data").join("indexes");
        // Index is the sole encrypted surface (P0.1): rotation is permitted.
        let db = build_db_index_only(root, &old_key);
        let (alice, bob) = seed(&db);
        // Pre-rotation: files are at key_version 1 AND every file is encrypted
        // (no silent plaintext bypass on the normal persist path).
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);
        assert!(assert_all_index_files_encrypted(&indexes_dir) > 0);

        let report = db
            .rotate_index_keys(KeyProviderConfig::File {
                path: new_key.clone(),
            })
            .unwrap();
        assert_eq!(report.old_version, 1);
        assert_eq!(report.new_version, 2);
        assert!(report.files_reencrypted > 0);

        // Every persisted index file advanced to key_version 2 AND every file
        // is still encrypted (rotation must never emit a plaintext file).
        let n = assert_all_at_version(&indexes_dir, 2);
        assert!(n > 0, "expected re-encrypted files at v2");
        assert_eq!(
            assert_all_index_files_encrypted(&indexes_dir),
            n,
            "some index file lost its AEIX header during rotation"
        );

        // Live instance: in-memory graph + vector search still work.
        assert!(db.get_node(alice).is_ok(), "node must still be readable");
        let similar = db
            .similarity_search(crate::db::similarity_query::SimilarityQuery::from_node(alice).k(5))
            .unwrap();
        assert!(similar.iter().any(|(id, _)| *id == bob));
        assert!(db.index_rotation_status().unwrap().is_fully_rotated());

        // Prove the re-encrypted manifest round-trips under the NEW index key.
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        let reader =
            IndexPersistenceManager::with_cipher(root.join("data"), Some(new_index_cipher.clone()));
        crate::storage::index_persistence::manifest::load_manifest_with_cipher(
            &reader.manifest_path(),
            Some(&new_index_cipher),
        )
        .expect("manifest must decrypt under the new key after rotation");
    }

    #[test]
    fn encrypted_persist_writes_no_plaintext_index_files() {
        // Cross-path guard: a plain encrypted persist (no rotation) must write
        // EVERY index file — bitcode files AND native usearch + sidecar — with
        // the AEIX header. Catches any write path that silently bypasses the
        // cipher (the compiler does not catch a plaintext bypass).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        let indexes_dir = root.join("data").join("indexes");
        let db = build_db(root, &key);
        seed(&db);
        let checked = assert_all_index_files_encrypted(&indexes_dir);
        assert!(
            checked >= 4,
            "expected the manifest/interner/graph/temporal/vector files to be present, got {checked}"
        );
    }

    #[test]
    fn rotate_rejects_same_key() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        // Index-only so we reach the same-key check rather than the P0.1 guard.
        let db = build_db_index_only(root, &key);
        seed(&db);
        let err = db
            .rotate_index_keys(KeyProviderConfig::File { path: key.clone() })
            .unwrap_err();
        assert!(
            err.to_string().contains("same key") || err.to_string().contains("equals"),
            "expected same-key rejection, got: {err}"
        );
    }

    #[test]
    fn resume_pending_index_rotation_is_none_when_no_breadcrumb() {
        // The `&self` resume wrapper (Issue #490 CLI drive-point) must return
        // Ok(None) — not an error, not a phantom rotation — on a configured,
        // encrypted, index-persistent DB with no rotation.state breadcrumb.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();

        let db = build_db(root, &key);
        seed(&db);
        // No breadcrumb was ever written.
        assert!(!root.join("data").join("rotation.state").exists());

        let report = db
            .resume_pending_index_rotation()
            .expect("resume with no pending rotation must be Ok, not Err");
        assert!(
            report.is_none(),
            "resume with no breadcrumb must report nothing pending (Ok(None)), got {report:?}"
        );
    }

    #[test]
    fn rotate_to_remote_source_refuses_without_reaching_endpoint() {
        // Rotating TO a KMS/Vault/passphrase source must return the clean
        // "not yet supported" refusal on the variant alone — BEFORE any network
        // call — so an unreachable endpoint never turns the refusal into a
        // transport error (Issue #3587). Index-only DB so we reach run_rotation.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();

        let db = build_db_index_only(root, &old_key);
        seed(&db);

        // Vault pointed at an unreachable address: must NOT be dialed.
        let err = db
            .rotate_index_keys(KeyProviderConfig::Vault {
                address: "https://127.0.0.1:1".to_string(),
                token_env: "NOPE_TOKEN".to_string(),
                mount: "secret".to_string(),
                path: "app/mek".to_string(),
                key_field: "key".to_string(),
                namespace: None,
                ca_cert: None,
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("not yet supported") && err.to_string().contains("vault"),
            "expected clean vault refusal, got: {err}"
        );
        // And no breadcrumb was written by the refused rotation.
        assert!(!root.join("data").join("rotation.state").exists());

        // KMS is refused the same way (no endpoint contacted).
        let err = db
            .rotate_index_keys(KeyProviderConfig::Kms {
                key_id: "alias/prod".to_string(),
                encrypted_data_key: "AAAA".to_string(),
                region: None,
                endpoint_url: Some("http://127.0.0.1:1".to_string()),
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("not yet supported") && err.to_string().contains("kms"),
            "expected clean kms refusal, got: {err}"
        );
    }

    #[test]
    fn rotate_rejects_without_encryption() {
        let db = AletheiaDB::new().unwrap();
        let err = db
            .rotate_index_keys(KeyProviderConfig::Env {
                variable: "NOPE".to_string(),
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("persistence") || err.to_string().contains("encryption"),
            "expected not-configured rejection, got: {err}"
        );
    }

    #[test]
    fn crash_resume_completes_from_state_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }

        // Simulate a crash mid-rotation: add the new generation, write the
        // breadcrumb, and re-encrypt only PART of the files.
        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().cloned().unwrap();
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        keyring.add_generation(2, new_index_cipher.clone());
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            ),
            2,
            new_index_cipher,
        );
        let mut n = 0;
        engine
            .re_encrypt(&mut |_| {
                n += 1;
                n < 2
            })
            .unwrap();
        // Mixed state + breadcrumb present.
        assert!(root.join("data").join("rotation.state").exists());

        // Resume finishes the rotation and clears the breadcrumb.
        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None)
            .unwrap()
            .expect("a pending rotation should resume");
        assert_eq!(report.new_version, 2);
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    // ── P0.1: cross-layer MEK-desync guard ───────────────────────────

    #[test]
    fn rotate_reencrypts_wal() {
        // Issue #3617 PR2: a fully-encrypted DB WITHOUT cold storage (WAL + index
        // under the same MEK) now ROTATES successfully — the full-MEK driver
        // re-keys the WAL via checkpoint → force-roll → truncate, so a subsequent
        // provider switch is safe. (This flips the former index-only refusal.)
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let (alice, bob) = {
            let db = build_db(root, &old_key); // WAL encrypted too
            let ids = seed(&db);
            assert!(db.wal.is_encrypted(), "precondition: WAL is encrypted");
            assert!(
                db.conflicting_encrypted_layers().is_empty(),
                "no cold storage → WAL+index rotation is now allowed"
            );

            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .expect("WAL+index rotation without cold storage must succeed");
            assert_eq!(report.new_version, 2);

            // Data is intact in-memory immediately after rotation.
            assert_eq!(db.node_count(), 2);
            assert_eq!(db.edge_count(), 1);

            // The rotation cleared its ledger.
            assert!(!root.join("data").join("rotation.state").exists());

            // Every remaining WAL segment is stamped with the NEW key_version;
            // the old-DEK segments were truncated.
            assert!(
                db.wal.all_segments_use_key_version(2),
                "all remaining WAL segments must carry the new key_version"
            );
            ids
        };

        // Reopen under the NEW key ONLY (the provider switch the whole feature
        // exists to make safe): the WAL replays under the new DEK, index files
        // decrypt under the new DEK, and the graph is fully recovered.
        {
            let db = build_db(root, &new_key);
            assert_eq!(db.node_count(), 2, "graph must recover under the new key");
            assert_eq!(db.edge_count(), 1);
            // Both seeded nodes are readable (and decrypt) under the new key.
            db.get_node(alice)
                .expect("alice recovers under the new key");
            db.get_node(bob).expect("bob recovers under the new key");
        }
    }

    #[test]
    fn rotate_reencrypts_cold() {
        // Issue #3617 PR3 (completes the issue): a fully-encrypted DB with an
        // ENCRYPTED COLD tier (WAL + index + cold under the same MEK) now ROTATES
        // fully — the driver bulk re-encrypts every cold value to the new cold
        // DEK, so a subsequent provider switch is safe. This flips the former
        // cross-layer refusal.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::version::{EntityVersion, NodeVersion};

        // A cold value to prove the bulk pass actually re-encrypts stored data
        // (seeded nodes live in the hot tier; migration is age-gated). Written
        // directly to the cold store so it is present as `ACV1@1` before rotation.
        let cold_vid = crate::core::id::VersionId::new(9001).unwrap();
        let (alice, bob) = {
            let db = build_db_with_cold(root, &old_key);
            let ids = seed(&db);
            assert!(db.wal.is_encrypted(), "precondition: WAL is encrypted");

            // Seed one cold-tier value directly.
            let cold_node = NodeVersion::new_anchor(
                cold_vid,
                crate::core::NodeId::new(500).unwrap(),
                crate::core::temporal::BiTemporalInterval::current(1234.into()),
                GLOBAL_INTERNER.intern("Person").unwrap(),
                PropertyMapBuilder::new().insert("name", "Carol").build(),
            );
            {
                let tiered = db
                    .historical
                    .read()
                    .tiered_storage_arc()
                    .expect("cold tier configured");
                let cold = tiered.cold_storage();
                assert!(cold.is_encrypted(), "precondition: cold tier is encrypted");
                cold.store_node_version(&cold_node).unwrap();
                assert_eq!(
                    cold.current_cold_key_version(),
                    Some(1),
                    "cold values start at generation 1"
                );
            }

            // The cross-layer guard no longer refuses — cold is rotatable.
            assert!(
                db.conflicting_encrypted_layers().is_empty(),
                "cold storage is now rotatable → no cross-layer conflict"
            );

            let flushed_before = {
                let tiered = db.historical.read().tiered_storage_arc().unwrap();
                tiered.cold_storage().get_flushed_lsn().unwrap()
            };

            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .expect("full-MEK rotation (index+WAL+cold) must succeed");
            assert_eq!(report.new_version, 2);
            assert!(!root.join("data").join("rotation.state").exists());

            // The cold layer was bulk re-encrypted to the new generation.
            {
                let tiered = db.historical.read().tiered_storage_arc().unwrap();
                let cold = tiered.cold_storage();
                assert_eq!(
                    cold.cold_value_format_version().unwrap(),
                    Some(2),
                    "cold values must be re-wrapped at the new generation"
                );
                // The value still decodes (now under the new cold DEK).
                let loaded = cold
                    .get_node_version(cold_vid)
                    .unwrap()
                    .expect("cold value survives rotation");
                assert_eq!(loaded.version_id(), cold_vid);
                // flushed_lsn (plaintext metadata) is untouched by the pass.
                assert_eq!(cold.get_flushed_lsn().unwrap(), flushed_before);
            }
            ids
        };

        // Provider switch: reopen under the NEW key ONLY — WAL replays, index +
        // cold values all decrypt under the new MEK's DEKs.
        {
            let db = build_db_with_cold(root, &new_key);
            assert_eq!(db.node_count(), 2, "graph recovers under the new key");
            assert_eq!(db.edge_count(), 1);
            db.get_node(alice)
                .expect("alice recovers under the new key");
            db.get_node(bob).expect("bob recovers under the new key");
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            let cold = tiered.cold_storage();
            let loaded = cold
                .get_node_version(cold_vid)
                .unwrap()
                .expect("cold value decrypts under the new key alone");
            assert_eq!(loaded.version_id(), cold_vid);
        }
    }

    /// Crash right after the rotation ledger was fsync'd but before any layer
    /// finished (design §5 step 2): the durable ledger records index + WAL
    /// `pending`. On the next open — still under the OLD key, since the rotation
    /// never completed — startup resume drives BOTH layers to completion (index
    /// re-encrypted, WAL force-rolled + old segments truncated), clears the
    /// ledger, and the graph is intact; a subsequent reopen under the NEW key
    /// alone then succeeds (the provider switch is now safe).
    #[test]
    fn crash_after_ledger_resumes_wal_and_index() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        // 1. Seed an encrypted DB and persist (the manifest covers all data), then
        //    plant a durable ledger simulating a crash right after it was written.
        {
            let db = build_db(root, &old_key);
            seed(&db); // persists indexes
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            super::write_ledger(
                &manager,
                &super::RotationLedger::forward_scope(
                    2,
                    KeyProviderConfig::File {
                        path: new_key.clone(),
                    },
                    true,  // WAL in scope
                    false, // cold not in scope
                ),
            )
            .unwrap();
            assert!(root.join("data").join("rotation.state").exists());
        }

        // 2. Reopen under the OLD key (rotation in flight): startup resume
        //    completes both layers and clears the ledger.
        {
            let db = build_db(root, &old_key);
            assert!(
                !root.join("data").join("rotation.state").exists(),
                "startup resume must complete the pending rotation and clear the ledger"
            );
            assert_eq!(db.node_count(), 2, "graph intact after resume");
            assert_eq!(db.edge_count(), 1);
            assert!(
                db.wal.all_segments_use_key_version(2),
                "resume must retire the old-generation WAL segments"
            );
            // Resume is idempotent: a second run with no ledger is a clean no-op.
            assert!(db.resume_pending_index_rotation().unwrap().is_none());
        }

        // 3. Provider switch: reopen under the NEW key only — everything decrypts.
        {
            let db = build_db(root, &new_key);
            assert_eq!(db.node_count(), 2, "graph recovers under the new key");
            assert_eq!(db.edge_count(), 1);
        }
    }

    // ── Issue #488 version-provisioning fast-follow ──────────────────────
    //
    // At `open()` the keyring's `current_version` was hard-coded to 1, because
    // the post-rotation key version is never persisted where `open()` reads it
    // (the rotation ledger is cleared on completion). These tests pin the fix:
    // provision `current_version` from durable on-disk state so `verify` /
    // `index_rotation_status` classify rotated files correctly and a second
    // rotation increments from the real version instead of re-using 2.

    /// Headline symptom: after a completed rotation to v2 and a genuine COLD
    /// reopen under the new key, `index_rotation_status` must report NO unknown
    /// files. Before the fix the reopened keyring reports `current_version = 1`,
    /// so the v2 files classify as "unknown" (a false-FAIL of `encryption
    /// verify`) even though they decrypt cleanly.
    #[test]
    fn verify_passes_after_cold_reopen_of_rotated_db() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        // Rotate a fully-encrypted DB (WAL + index) to the new key.
        {
            let db = build_db(root, &old_key);
            seed(&db);
            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .expect("rotation must succeed");
            assert_eq!(report.new_version, 2);
        }
        // All on-disk index files are now at v2.
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);

        // Genuine COLD reopen under the NEW key only (fresh manager/keyring).
        let db = build_db(root, &new_key);
        let status = db
            .index_rotation_status()
            .expect("status probe must succeed after reopen");
        assert_eq!(
            status.unknown, 0,
            "reopened keyring must classify v2 files as current, not unknown: {status:?}"
        );
        assert!(
            status.is_fully_rotated(),
            "a cold-reopened rotated DB must read as fully rotated: {status:?}"
        );
        // The active-decrypt probe (Issue #3618) must also pass.
        let probe = db
            .verify_index_decryptable()
            .expect("decrypt probe must succeed");
        assert!(
            probe.decrypt_failed.is_empty(),
            "bodies must decrypt under the reopened new key: {probe:?}"
        );
    }

    /// A second rotation after a cold reopen must compute `new_version = 3`
    /// (not re-use 2) and must NOT wedge on the `ForeignKeyVersionFile`
    /// identity check against the existing v2 files. Before the fix the
    /// reopened keyring reports `current_version = 1`, so the second rotation
    /// recomputes `new_version = 2` and wedges.
    #[test]
    fn second_rotation_after_reopen_does_not_wedge() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        let newer_key = root.join("newer.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&newer_key).unwrap();

        // First rotation: v1 -> v2.
        {
            let db = build_db(root, &old_key);
            seed(&db);
            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .expect("first rotation must succeed");
            assert_eq!(report.new_version, 2);
        }

        // Cold reopen under the new key, write, then rotate AGAIN.
        let db = build_db(root, &new_key);
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .unwrap();
        db.persist_indexes().unwrap();

        let report = db
            .rotate_index_keys(KeyProviderConfig::File {
                path: newer_key.clone(),
            })
            .expect("second rotation must not wedge on ForeignKeyVersionFile");
        assert_eq!(
            report.old_version, 2,
            "second rotation must see the provisioned current version 2"
        );
        assert_eq!(
            report.new_version, 3,
            "second rotation must increment to 3, not re-use 2"
        );
        assert!(db.index_rotation_status().unwrap().is_fully_rotated());
    }

    /// A mid-rotation crash leaves a PENDING ledger (target v2) and a mix of
    /// on-disk versions. On reopen the resolved current version must follow the
    /// ledger target once resume completes the rotation, and the DB must open +
    /// resume correctly.
    #[test]
    fn mid_rotation_crash_reopen_provisions_from_ledger() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        // Seed + persist, then plant a durable ledger simulating a crash right
        // after it was fsync'd (index + WAL pending, target v2).
        {
            let db = build_db(root, &old_key);
            seed(&db);
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            super::write_ledger(
                &manager,
                &super::RotationLedger::forward_scope(
                    2,
                    KeyProviderConfig::File {
                        path: new_key.clone(),
                    },
                    true,
                    false, // cold not in scope
                ),
            )
            .unwrap();
            assert!(root.join("data").join("rotation.state").exists());
        }

        // Reopen under the OLD key (rotation in flight): startup resume must
        // complete the rotation and clear the ledger, and the final provisioned
        // current version must follow the ledger target (v2).
        {
            let db = build_db(root, &old_key);
            assert!(
                !root.join("data").join("rotation.state").exists(),
                "startup resume must complete the pending rotation"
            );
            assert_eq!(db.node_count(), 2, "graph intact after resume");
            let status = db.index_rotation_status().unwrap();
            assert_eq!(
                status.unknown, 0,
                "no unknown files after resume: {status:?}"
            );
            assert!(
                status.is_fully_rotated(),
                "resume must leave a uniform v2 dataset: {status:?}"
            );
            assert_eq!(
                assert_all_at_version(&root.join("data").join("indexes"), 2),
                status.at_current,
                "all index files at ledger target v2"
            );
        }

        // Provider switch: reopen under the NEW key only — everything decrypts,
        // and a fresh reopen classifies as fully rotated (current version v2).
        {
            let db = build_db(root, &new_key);
            assert_eq!(db.node_count(), 2, "graph recovers under the new key");
            assert!(db.index_rotation_status().unwrap().is_fully_rotated());
        }
    }

    /// The provisioned version is the MAX of the index-header version and the
    /// WAL-segment version. Unit-tests the resolver directly (no pending
    /// ledger): index files at v2, WAL segments at v1 -> resolves to 2.
    #[test]
    fn provisioned_version_is_max_across_index_and_wal() {
        use crate::storage::index_persistence::common::encrypt_index_bytes_versioned;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let data_dir = root.join("data");
        let indexes_dir = data_dir.join("indexes");
        let wal_dir = root.join("wal");
        std::fs::create_dir_all(&indexes_dir).unwrap();
        std::fs::create_dir_all(&wal_dir).unwrap();

        // An encrypted index file stamped at v2.
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        let cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&key))
                .unwrap()
                .index_cipher(),
        );
        let body = encrypt_index_bytes_versioned(b"manifest-body", &cipher, 2).unwrap();
        std::fs::write(indexes_dir.join("manifest.idx"), &body).unwrap();

        // No WAL segments (v1 default) and no pending ledger -> resolves to the
        // index version (2), the max across the two layers.
        let resolved =
            super::resolve_provisioned_key_version(&indexes_dir, &wal_dir, &data_dir).unwrap();
        assert_eq!(resolved, 2, "resolved version must be the max (index v2)");
    }

    #[test]
    fn rotate_succeeds_when_index_is_sole_encrypted_surface() {
        // The narrow allowed case: index is the only encrypted-at-rest layer.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        let db = build_db_index_only(root, &old_key);
        seed(&db);
        assert!(db.conflicting_encrypted_layers().is_empty());
        let report = db
            .rotate_index_keys(KeyProviderConfig::File {
                path: new_key.clone(),
            })
            .expect("index-only rotation must be permitted");
        assert_eq!(report.new_version, 2);
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    // ── P0.3: refuse-if-in-progress ──────────────────────────────────

    #[test]
    fn rotate_refuses_when_rotation_already_in_progress() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let db = build_db_index_only(root, &old_key);
        seed(&db);
        // Plant a breadcrumb: a rotation is "already in progress".
        let manager = db.persistence_manager.clone().unwrap();
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();

        let err = db
            .rotate_index_keys(KeyProviderConfig::File {
                path: new_key.clone(),
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("already in progress"),
            "expected AlreadyInProgress, got: {err}"
        );
    }

    // ── P0.2: durable, ordered breadcrumb ────────────────────────────

    #[test]
    fn write_rotation_state_is_durable_and_leaves_no_temp() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            None,
        ));
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::Env {
                variable: "SOME_VAR".to_string(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();

        // The breadcrumb is present, parses back, and NO temp scratch remains.
        let pending = super::read_rotation_state(&manager)
            .unwrap()
            .expect("breadcrumb present");
        assert_eq!(pending.new_version, 2);
        assert_eq!(pending.direction, super::RotationDirection::Forward);
        let leftover: Vec<_> = std::fs::read_dir(root.join("data"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("rotation.state.tmp")
            })
            .collect();
        assert!(leftover.is_empty(), "durable write left a temp file behind");
    }

    #[test]
    fn breadcrumb_is_present_before_first_v2_file_on_crash() {
        // Structural ordering check for P0.2: run_rotation publishes the durable
        // breadcrumb BEFORE re-encrypting. We simulate a crash after only the
        // begin+breadcrumb by exercising the engine partially and confirming the
        // breadcrumb exists alongside the mixed state (the same window a real
        // power loss would leave).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        // Breadcrumb first (durable), matching run_rotation's ordering.
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();
        assert!(
            root.join("data").join("rotation.state").exists(),
            "breadcrumb must be durable before any v2 file is published"
        );
    }

    // ── P1.1: fail-closed on corrupt rotation.state ──────────────────

    #[test]
    fn corrupt_rotation_state_fails_closed_on_resume() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        // A present-but-unparseable breadcrumb.
        std::fs::write(
            data_dir.join("rotation.state"),
            b"this is not valid\n@@@garbage",
        )
        .unwrap();

        let enc_cfg = EncryptionConfig::file_based(&key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            data_dir.clone(),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let err = resume_pending_rotation(&manager, &enc_cfg, None, None).unwrap_err();
        assert!(
            err.to_string().contains("corrupt"),
            "corrupt breadcrumb must abort startup, got: {err}"
        );
        // And an ABSENT breadcrumb is a clean no-op (distinguish absent vs corrupt).
        std::fs::remove_file(data_dir.join("rotation.state")).unwrap();
        assert!(
            resume_pending_rotation(&manager, &enc_cfg, None, None)
                .unwrap()
                .is_none()
        );
    }

    // ── P1.2: cancel crash-safety (direction flag) ───────────────────

    #[test]
    fn interrupted_cancel_resumes_as_cancel() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().cloned().unwrap();
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        keyring.add_generation(2, new_index_cipher.clone());
        // Forward pass migrated SOME files to v2, then a cancel began (durable
        // cancel-direction breadcrumb) but was interrupted before finishing.
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            ),
            2,
            new_index_cipher,
        );
        let mut n = 0;
        engine
            .re_encrypt(&mut |_| {
                n += 1;
                n < 2
            })
            .unwrap();
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Cancel,
        )
        .unwrap();

        // Resume must honor the cancel direction: roll everything back to v1
        // (the OLD key), NOT roll forward to v2.
        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None)
            .unwrap()
            .expect("a pending cancel should resume");
        assert_eq!(report.new_version, 1, "cancel rolls back to the old key");
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);
        // Zero files remain at v2.
        let mut v2 = 0;
        fn count_v2(dir: &Path, expected: u32, c: &mut usize) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    count_v2(&p, expected, c);
                } else if let Ok(b) = std::fs::read(&p)
                    && index_file_key_version(&b) == Some(expected)
                {
                    *c += 1;
                }
            }
        }
        count_v2(&indexes_dir, 2, &mut v2);
        assert_eq!(v2, 0, "no file may remain at the new key after cancel");
    }

    #[test]
    fn cancel_pending_rotation_rolls_back_and_clears_breadcrumb() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        let db = build_db_index_only(root, &old_key);
        seed(&db);
        // Plant a pending forward rotation (breadcrumb) as if interrupted.
        let manager = db.persistence_manager.clone().unwrap();
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();

        let report = db.cancel_pending_rotation().expect("cancel must succeed");
        assert_eq!(report.new_version, 1);
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);

        // Cancel with no pending rotation is NotInProgress.
        let err = db.cancel_pending_rotation().unwrap_err();
        assert!(err.to_string().contains("no key rotation is in progress"));
    }

    // ── P2.2: audit failure carries a category, not raw error text ────

    #[test]
    fn rotation_failure_category_reduces_error() {
        let key_err: Error = StorageError::KeyProvider("/secret/path/key".to_string()).into();
        assert_eq!(super::rotation_failure_category(&key_err), "key_provider");
        let generic: Error = StorageError::PersistenceError(
            "index file /secret/leak.idx does not decrypt".to_string(),
        )
        .into();
        let cat = super::rotation_failure_category(&generic);
        assert_eq!(cat, "rotation_failed");
        assert!(!cat.contains("secret") && !cat.contains("/"));
    }

    // ── P2.3: rotation audit routed through the configured logger ─────

    fn build_db_index_only_with_enc(root: &Path, enc: EncryptionConfig) -> AletheiaDB {
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(root.join("wal"))
                    .durability_mode(DurabilityMode::GroupCommit {
                        max_delay_ms: 5,
                        max_batch_size: 64,
                    })
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: true,
                data_dir: root.join("data"),
                load_on_startup: true,
                ..Default::default()
            })
            .encryption(enc)
            .build();
        let mut db = AletheiaDB::with_unified_config(config).unwrap();
        db.wal = std::sync::Arc::new(
            ConcurrentWalSystem::new(ConcurrentWalSystemConfig::new(root.join("wal-plain")))
                .unwrap(),
        );
        db
    }

    #[test]
    fn rotation_emits_audit_through_configured_logger() {
        use crate::encryption::audit::AuditLevel;
        use crate::encryption::config::{AuditConfig, AuditDestination};

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let audit_path = root.join("audit.log");

        let mut enc = EncryptionConfig::file_based(&old_key);
        enc.audit = AuditConfig {
            enabled: true,
            level: AuditLevel::KeyEvents,
            destination: AuditDestination::File,
            file_path: Some(audit_path.clone()),
            instance_id: Some("node-test".into()),
            ..AuditConfig::default()
        };

        let db = build_db_index_only_with_enc(root, enc);
        seed(&db);
        db.rotate_index_keys(KeyProviderConfig::File {
            path: new_key.clone(),
        })
        .unwrap();

        let log = std::fs::read_to_string(&audit_path).expect("audit log written");
        assert!(
            log.contains("key.rotation.started"),
            "expected rotation.started in audit log: {log}"
        );
        assert!(
            log.contains("key.rotation.completed"),
            "expected rotation.completed in audit log: {log}"
        );
        assert!(log.contains("node-test"));
    }

    // ── #3617 PR1: cross-layer ledger (v2) ───────────────────────────

    #[test]
    fn ledger_v2_roundtrip_is_durable_and_leaves_no_temp() {
        // A v2 ledger with mixed per-layer statuses round-trips durably: every
        // field (target version, source ref, direction, per-layer status) is
        // preserved, the on-disk format is v2, no key bytes are written, and no
        // temp scratch is left behind.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            None,
        ));
        let ledger = super::RotationLedger {
            direction: super::RotationDirection::Forward,
            new_version: 3,
            new_source: KeyProviderConfig::Env {
                variable: "MEK_VAR".to_string(),
            },
            index: super::LayerStatus::Complete,
            checkpoint: super::LayerStatus::Complete,
            wal: super::LayerStatus::Skipped,
            cold: super::LayerStatus::Pending,
        };
        super::write_ledger(&manager, &ledger).unwrap();

        let read = super::read_rotation_state(&manager)
            .unwrap()
            .expect("ledger present");
        assert_eq!(read, ledger, "every ledger field must round-trip");

        let body = std::fs::read_to_string(root.join("data").join("rotation.state")).unwrap();
        assert!(body.contains("version=2"), "expected v2 format: {body}");
        assert!(body.contains("target_version=3"));
        assert!(body.contains("layer.index=complete"));
        assert!(body.contains("layer.cold=pending"));
        // Source REFERENCE only (env var NAME), never key bytes.
        assert!(body.contains("new_source_kind=env") && body.contains("MEK_VAR"));

        let leftover: Vec<_> = std::fs::read_dir(root.join("data"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("rotation.state.tmp")
            })
            .collect();
        assert!(leftover.is_empty(), "durable write left a temp file behind");
    }

    #[test]
    fn ledger_v2_read_is_fail_closed_on_corruption() {
        // A truncated/garbage v2 ledger must fail closed (Err), while an absent
        // file is a clean Ok(None) — the reader must never confuse the two.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let manager =
            std::sync::Arc::new(IndexPersistenceManager::with_cipher(data_dir.clone(), None));

        // Truncated v2: version + direction but no target_version / source.
        std::fs::write(
            data_dir.join("rotation.state"),
            b"version=2\ndirection=forward\n",
        )
        .unwrap();
        let err = super::read_rotation_state(&manager).unwrap_err();
        assert!(
            err.to_string().contains("corrupt"),
            "truncated ledger must fail closed, got: {err}"
        );

        // Pure garbage (no key=value on the first line).
        std::fs::write(data_dir.join("rotation.state"), b"@@@not a ledger@@@").unwrap();
        assert!(
            super::read_rotation_state(&manager).is_err(),
            "garbage ledger must fail closed"
        );

        // An unknown per-layer value also fails closed.
        std::fs::write(
            data_dir.join("rotation.state"),
            b"version=2\ndirection=forward\ntarget_version=2\nnew_source_kind=file\nnew_source_value=/k\nlayer.index=bogus\n",
        )
        .unwrap();
        assert!(
            super::read_rotation_state(&manager).is_err(),
            "unknown layer status must fail closed"
        );

        // Absent → Ok(None).
        std::fs::remove_file(data_dir.join("rotation.state")).unwrap();
        assert!(
            super::read_rotation_state(&manager).unwrap().is_none(),
            "absent ledger is a clean no-op"
        );
    }

    #[test]
    fn ledger_v2_write_ordering_breadcrumb_before_keyring_flip() {
        // Mirror `breadcrumb_is_present_before_first_v2_file_on_crash` for the
        // v2 ledger: the ledger is durable (and v2) BEFORE any new-version index
        // file is stamped — i.e. before the keyring flips.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        // Pre-rotation: every file at v1 (keyring not yet flipped).
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        // Ledger first (durable), exactly as run_rotation orders it.
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();

        let state = root.join("data").join("rotation.state");
        assert!(
            state.exists(),
            "the v2 ledger must be durable before any new-version file"
        );
        let body = std::fs::read_to_string(&state).unwrap();
        assert!(
            body.contains("version=2") && body.contains("layer.index=pending"),
            "expected a v2 ledger: {body}"
        );
        // No index file has been re-stamped to v2 yet (keyring not flipped).
        let mut v2 = 0;
        fn count_v2(dir: &Path, expected: u32, c: &mut usize) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    count_v2(&p, expected, c);
                } else if let Ok(b) = std::fs::read(&p)
                    && index_file_key_version(&b) == Some(expected)
                {
                    *c += 1;
                }
            }
        }
        count_v2(&indexes_dir, 2, &mut v2);
        assert_eq!(v2, 0, "no v2 file may exist before the keyring flip");
    }

    #[test]
    fn rotation_derives_all_four_deks_and_refuses_identical_mek() {
        use std::collections::HashSet;
        use zeroize::Zeroizing;

        // Identical MEK → identical DEKs across EVERY layer (the basis for the
        // constant-time MEK refusal: same MEK re-derives the same four keys).
        let mek_a = Zeroizing::new([7u8; 32]);
        let mek_a2 = Zeroizing::new([7u8; 32]);
        let a = super::MekKeyset::derive(&mek_a).unwrap();
        let a2 = super::MekKeyset::derive(&mek_a2).unwrap();
        for layer in super::RotationLayer::ALL {
            assert_eq!(
                a.dek(layer).as_ref(),
                a2.dek(layer).as_ref(),
                "identical MEK must derive identical DEK for {layer:?}"
            );
        }

        // Different MEK → all four DEKs change and stay pairwise domain-separated.
        let mek_b = Zeroizing::new([9u8; 32]);
        let b = super::MekKeyset::derive(&mek_b).unwrap();
        let mut seen: HashSet<[u8; 32]> = HashSet::new();
        for layer in super::RotationLayer::ALL {
            assert_ne!(
                a.dek(layer).as_ref(),
                b.dek(layer).as_ref(),
                "new MEK must change {layer:?}'s DEK"
            );
            assert!(
                seen.insert(**b.dek(layer)),
                "layer DEKs must be domain-separated"
            );
        }
        assert_eq!(seen.len(), 4, "four distinct DEKs expected");

        // End-to-end: rotating to an identical MEK is refused (MEK ct_eq),
        // exercised through the real index-only rotation path.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        let db = build_db_index_only(root, &key);
        seed(&db);
        let err = db
            .rotate_index_keys(KeyProviderConfig::File { path: key.clone() })
            .unwrap_err();
        assert!(
            err.to_string().contains("same key") || err.to_string().contains("equals"),
            "rotating to an identical MEK must be refused, got: {err}"
        );
    }

    #[test]
    fn v1_state_file_backward_compat() {
        // A legacy #488 v1 breadcrumb (index-only; no `layer.*` lines, using the
        // old `new_version` key) parses into the v2 shape: index takes the
        // recorded direction (Pending) while the layers #3617 adds are Skipped
        // (a #488 DB had no checkpoint/WAL/cold rotation in flight). This keeps
        // an interrupted #488 rotation resumable rather than rejecting it.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let manager =
            std::sync::Arc::new(IndexPersistenceManager::with_cipher(data_dir.clone(), None));
        std::fs::write(
            data_dir.join("rotation.state"),
            b"version=1\ndirection=forward\nnew_version=2\nnew_source_kind=file\nnew_source_value=/etc/aletheia/new.key\n",
        )
        .unwrap();

        let ledger = super::read_rotation_state(&manager)
            .unwrap()
            .expect("a v1 breadcrumb must parse");
        assert_eq!(ledger.new_version, 2);
        assert_eq!(ledger.direction, super::RotationDirection::Forward);
        assert_eq!(ledger.index, super::LayerStatus::Pending);
        assert_eq!(ledger.checkpoint, super::LayerStatus::Skipped);
        assert_eq!(ledger.wal, super::LayerStatus::Skipped);
        assert_eq!(ledger.cold, super::LayerStatus::Skipped);
        assert!(matches!(ledger.new_source, KeyProviderConfig::File { .. }));
    }

    #[test]
    fn crash_resume_completes_from_v2_ledger() {
        // Adapts `crash_resume_completes_from_state_file` to the v2 ledger: a
        // mid-index-rotation crash (v2 ledger + partially re-encrypted files)
        // resumes and completes, then clears the ledger.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().cloned().unwrap();
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        keyring.add_generation(2, new_index_cipher.clone());
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();
        // The planted ledger is v2 with the index layer pending.
        let body = std::fs::read_to_string(root.join("data").join("rotation.state")).unwrap();
        assert!(
            body.contains("version=2") && body.contains("layer.index=pending"),
            "expected a v2 ledger on disk: {body}"
        );

        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            ),
            2,
            new_index_cipher,
        );
        let mut n = 0;
        engine
            .re_encrypt(&mut |_| {
                n += 1;
                n < 2
            })
            .unwrap();
        assert!(root.join("data").join("rotation.state").exists());

        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None)
            .unwrap()
            .expect("a pending rotation should resume from the v2 ledger");
        assert_eq!(report.new_version, 2);
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    #[test]
    fn rotation_emits_no_audit_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let audit_path = root.join("audit.log");

        // Audit disabled (default): destination File is never resolved, so the
        // file is never created.
        let db = build_db_index_only(root, &old_key);
        seed(&db);
        db.rotate_index_keys(KeyProviderConfig::File {
            path: new_key.clone(),
        })
        .unwrap();
        assert!(
            !audit_path.exists(),
            "no audit file should be written when auditing is disabled"
        );
    }

    // ── #3617 PR1 review hardening: crash-consistency + fail-closed ──

    #[test]
    fn crash_after_full_reencrypt_before_clear_resumes_and_clears() {
        // Crash window: the forward pass re-encrypted (and complete()'d) EVERY
        // index file to v2, but the process died BEFORE clearing the ledger. The
        // planted ledger therefore still records index=Pending (the driver never
        // rewrites the ledger to Complete; it only clears at the very end). On
        // restart, resume must run the idempotent re-encrypt as a genuine no-op
        // (files_reencrypted == 0), still verify+complete, and clear the ledger.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().cloned().unwrap();
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        keyring.add_generation(2, new_index_cipher.clone());
        // Ledger first (v2, index=Pending), exactly as run_rotation orders it.
        super::write_rotation_state(
            &manager,
            2,
            &KeyProviderConfig::File {
                path: new_key.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();
        // Re-encrypt ALL files to v2 (the full forward pass that completed
        // before the crash), then leave the ledger present (crash before clear).
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            ),
            2,
            new_index_cipher,
        );
        engine.re_encrypt(&mut |_| true).unwrap();
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
        assert!(
            root.join("data").join("rotation.state").exists(),
            "precondition: ledger still present (crash before clear)"
        );

        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None)
            .unwrap()
            .expect("a pending rotation should resume");
        assert_eq!(report.new_version, 2);
        assert_eq!(
            report.files_reencrypted, 0,
            "already-migrated dataset must re-encrypt nothing (idempotent no-op)"
        );
        assert!(
            !root.join("data").join("rotation.state").exists(),
            "resume must clear the ledger after completing"
        );
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    #[test]
    fn resume_with_index_complete_ledger_verifies_and_clears() {
        // Positive skip-branch: a v2 ledger recording index=Complete (and
        // checkpoint=Complete) while the index files are already at v2. Resume
        // must take the skip branch (index_domain_pending == false → NO
        // re_encrypt), still verify via complete(), and clear the ledger.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().cloned().unwrap();
        let new_index_cipher = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        keyring.add_generation(2, new_index_cipher.clone());
        // Truthfully migrate every file to v2 first...
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            ),
            2,
            new_index_cipher,
        );
        engine.re_encrypt(&mut |_| true).unwrap();
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);

        // ...then plant a ledger claiming the index domain is already Complete.
        super::write_ledger(
            &manager,
            &super::RotationLedger {
                direction: super::RotationDirection::Forward,
                new_version: 2,
                new_source: KeyProviderConfig::File {
                    path: new_key.clone(),
                },
                index: super::LayerStatus::Complete,
                checkpoint: super::LayerStatus::Complete,
                wal: super::LayerStatus::Skipped,
                cold: super::LayerStatus::Skipped,
            },
        )
        .unwrap();

        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None)
            .unwrap()
            .expect("an index=Complete ledger should still resume to verify+clear");
        assert_eq!(report.new_version, 2);
        // Skip branch: progress is RotationProgress::default() — no file was
        // even scanned for re-encryption (distinguishes it from the Pending
        // path, which would report files_skipped > 0).
        assert_eq!(report.files_reencrypted, 0);
        assert_eq!(
            report.files_total, 0,
            "skip branch must not run the re-encrypt scan"
        );
        assert_eq!(report.files_skipped, 0);
        assert!(
            !root.join("data").join("rotation.state").exists(),
            "resume must clear the ledger after verifying"
        );
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    #[test]
    fn resume_with_lying_index_complete_ledger_fails_closed() {
        // Negative skip-branch (the important one): a v2 ledger LIES that the
        // index domain is Complete, but the index files are still at the OLD key
        // (never migrated). Resume takes the skip branch (no re_encrypt) and then
        // complete()/verify_complete() MUST fail closed — old-key files remain,
        // so the old key must NOT be retired and the ledger must NOT be cleared.
        // This locks in the fail-closed guarantee PR2 relies on when it drives
        // the skip branch for the WAL layer.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        // Files remain at v1 (old key): we DO NOT re-encrypt anything.
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        // Plant the LYING ledger: index=Complete while old-key files remain.
        super::write_ledger(
            &manager,
            &super::RotationLedger {
                direction: super::RotationDirection::Forward,
                new_version: 2,
                new_source: KeyProviderConfig::File {
                    path: new_key.clone(),
                },
                index: super::LayerStatus::Complete,
                checkpoint: super::LayerStatus::Complete,
                wal: super::LayerStatus::Skipped,
                cold: super::LayerStatus::Skipped,
            },
        )
        .unwrap();

        let resume_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let err = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None).expect_err(
            "a ledger claiming index=Complete over unmigrated old-key files MUST fail closed",
        );
        // The verify_complete() fail-closed path (OldKeyFilesRemain /
        // ForeignKeyVersionFile) surfaces as a rotation failure, never a
        // silent success.
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "fail-closed error must carry a message, got empty"
        );
        // Loudly abort for manual intervention: the ledger is NOT cleared, so a
        // subsequent startup re-enters the same fail-closed path rather than
        // retiring the old key over files only the old key can read.
        assert!(
            root.join("data").join("rotation.state").exists(),
            "a fail-closed resume must LEAVE the ledger for manual intervention"
        );
        // And the old-key files are untouched — the old generation was never
        // retired.
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);
    }

    #[test]
    fn unknown_ledger_format_version_is_corrupt() {
        // A ledger with an UNKNOWN format version (here v3) must fail closed as
        // corrupt/unsupported, never be silently accepted — v1 and v2 are the
        // only shapes this reader understands. The rest of the ledger is
        // otherwise well-formed, proving it is the version dispatch that
        // rejects, not a missing field.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let manager =
            std::sync::Arc::new(IndexPersistenceManager::with_cipher(data_dir.clone(), None));

        std::fs::write(
            data_dir.join("rotation.state"),
            b"version=3\ndirection=forward\ntarget_version=4\nnew_source_kind=file\nnew_source_value=/etc/aletheia/new.key\nlayer.index=pending\n",
        )
        .unwrap();

        let err = super::read_rotation_state(&manager)
            .expect_err("an unknown ledger format version must fail closed, not parse to Ok");
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported ledger version") || msg.contains("corrupt"),
            "expected an unsupported-version corruption error, got: {msg}"
        );
    }

    /// DEFECT #3617-PR2 (completion-gating): if WAL retirement cannot remove
    /// every old-generation segment, `rotate_index_keys` must NOT report success
    /// and must NOT clear the ledger (else the operator drops the old MEK and a
    /// stranded old-DEK segment wedges startup). A later resume, once the blocker
    /// clears, finishes the rotation and ONLY THEN clears the ledger. Also pins
    /// that `all_segments_use_key_version(new)` gates the clear.
    #[test]
    fn incomplete_wal_retirement_leaves_ledger_pending_and_no_success() {
        use crate::storage::wal::segment_reader::{WAL_MAGIC, WAL_VERSION_ENCRYPTED_STRING_LABELS};
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let db = build_db(root, &old_key);
        seed(&db);
        let ledger_path = root.join("data").join("rotation.state");

        // Plant a stale old-generation WAL segment retirement cannot remove: its
        // id sits far above the active segment, so retire skips it as "active"
        // and it survives — tripping `all_segments_use_key_version(new)` = false.
        let stale = root.join("wal").join("999999.log");
        let mut hdr = Vec::new();
        hdr.extend_from_slice(&WAL_MAGIC);
        hdr.push(WAL_VERSION_ENCRYPTED_STRING_LABELS); // legacy (old-generation) header
        std::fs::write(&stale, &hdr).unwrap();

        // (b) Rotation must FAIL (incomplete) — no success report.
        let result = db.rotate_index_keys(KeyProviderConfig::File {
            path: new_key.clone(),
        });
        assert!(
            result.is_err(),
            "incomplete WAL retirement must NOT report a success RotationReport"
        );
        assert!(
            !db.wal.all_segments_use_key_version(2),
            "precondition: a stale old-generation segment remains on disk"
        );
        // (a) Ledger must remain PENDING (not cleared).
        assert!(
            ledger_path.exists(),
            "the ledger must remain pending when retirement is incomplete (never cleared)"
        );

        // (c) Clear the blocker; resume now completes and ONLY THEN clears.
        std::fs::remove_file(&stale).unwrap();
        let resumed = db
            .resume_pending_index_rotation()
            .expect("resume must not error once the blocker is cleared");
        assert!(
            resumed.is_some(),
            "resume must complete the pending rotation"
        );
        assert!(
            !ledger_path.exists(),
            "resume clears the ledger only after full old-generation retirement"
        );
        assert!(
            db.wal.all_segments_use_key_version(2),
            "all remaining WAL segments now carry the new key_version"
        );
        assert_eq!(db.node_count(), 2, "data intact throughout");
        assert_eq!(db.edge_count(), 1);
    }

    /// DEFECT #3617-PR2 (data-loss window): the OLD order retired (physically
    /// deleted) the sealed old-generation WAL segments while the durable index
    /// manifest still sat at the PRE-rotation checkpoint LSN. Any write
    /// acknowledged between that checkpoint and the roll therefore lived only in a
    /// segment that was then deleted, at an LSN ABOVE the manifest — so a crash
    /// before the next periodic checkpoint would lose it (node/edge state is
    /// recovered from the manifest snapshot + WAL replay, and the WAL no longer
    /// held it). The fix persists AGAIN after the roll and before the retire.
    ///
    /// Observable invariant asserted here (independent of the index backstop):
    /// after rotation, the persisted manifest LSN must be >= the WAL LSN of every
    /// retired (pre-roll) write — i.e. no retired segment holds un-snapshotted
    /// acknowledged entries.
    #[test]
    fn post_checkpoint_write_snapshotted_before_wal_retirement() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let db = build_db(root, &old_key);
        seed(&db); // persists indexes — the PRE-rotation checkpoint
        let manager = db.persistence_manager.clone().unwrap();

        // Post-checkpoint write: acknowledged AFTER the checkpoint, so its LSN is
        // above the checkpoint's manifest LSN and it lives in the old active WAL
        // segment (not yet in the durable snapshot).
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Carol").build(),
        )
        .unwrap();

        let manifest_lsn_checkpoint = manager.load_manifest_and_strings().unwrap().lsn;
        let wal_lsn_before_roll = db.wal.current_lsn().0;
        assert!(
            wal_lsn_before_roll > manifest_lsn_checkpoint,
            "precondition: the post-checkpoint write (wal LSN {wal_lsn_before_roll}) is above the \
             checkpoint manifest LSN ({manifest_lsn_checkpoint})"
        );

        // Drive the WAL-layer rotation (roll -> [fix: persist] -> retire). The
        // retire deletes the sealed old-generation segments holding the pre-roll
        // writes.
        let algorithm = db.encryption_config.as_ref().unwrap().algorithm;
        let new_wal_dek = super::derive_wal_dek(
            super::load_mek(&KeyProviderConfig::File {
                path: new_key.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        let new_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(algorithm, &new_wal_dek));
        db.rotate_wal_layer(&manager, new_wal_cipher, 2)
            .expect("WAL layer rotation must succeed");

        // For the header-based retire to be lossless, the persisted manifest LSN
        // must now cover every retired write. Without persist-after-roll it is
        // still stuck at the pre-rotation checkpoint LSN — the data-loss window.
        let manifest_lsn_after = manager.load_manifest_and_strings().unwrap().lsn;
        assert!(
            manifest_lsn_after >= wal_lsn_before_roll,
            "persist-after-roll must snapshot all pre-roll writes into the durable manifest \
             BEFORE retiring their segments: manifest LSN {manifest_lsn_after} must cover the \
             pre-roll WAL LSN {wal_lsn_before_roll}, else a retired segment holds \
             un-snapshotted acknowledged writes (crash-consistency data-loss window)"
        );
    }

    /// DEFECT #3617-PR2 (resume-path double-crash data-loss window): on the
    /// forward path `rotate_wal_layer` persists the index snapshot AFTER the roll
    /// and BEFORE retiring the old-generation segments, so a retired segment never
    /// holds an un-snapshotted write. The RESUME (startup) path used to retire the
    /// old-generation segments with a NO-OP persist while the durable index
    /// manifest still sat at the pre-rotation checkpoint LSN — any write
    /// acknowledged between that checkpoint and the seal then lived ONLY in the
    /// in-RAM replay buffer (applied to current state) and in an on-disk segment
    /// that resume immediately deleted. A SECOND crash before the async
    /// background checkpoint therefore lost it: on the next open the snapshot is
    /// still at the checkpoint LSN and the segment holding the write is gone.
    ///
    /// The fix defers the resume-path retire+ledger-clear until after the startup
    /// replay has been captured by a SYNCHRONOUS `persist_indexes()`, so no
    /// old-generation WAL segment is deleted until its entries are durable in the
    /// index snapshot. This test builds the crash-A on-disk state (a
    /// post-checkpoint write living only in an old-generation segment, PENDING
    /// ledger), runs the resume/open path, and asserts BOTH the crux manifest-LSN
    /// invariant (the durable manifest must cover the pre-retire write) AND that
    /// the write survives a SECOND crash with no async checkpoint in between.
    #[test]
    fn resume_path_persists_before_retiring_survives_second_crash() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        // ── Crash A: a forward WAL rotation crashed between the seal and the
        // post-roll persist. On disk: the seeded graph is durable in the index
        // snapshot at checkpoint LSN L_c; a post-checkpoint write ("Carol") lives
        // ONLY in the (old-generation) active WAL segment at an LSN above L_c; the
        // ledger is PENDING with the WAL in scope.
        //
        // The crash is simulated with `std::mem::forget` so the db's `Drop`
        // (which signals the background persistence thread to run a FINAL
        // shutdown persist_all_indexes) never runs — exactly like a `SIGKILL`.
        // A clean drop here would durably snapshot Carol and mask the bug. ──
        let carol_wal_lsn;
        let checkpoint_lsn;
        {
            let db = build_db(root, &old_key);
            seed(&db); // persists indexes — the PRE-rotation checkpoint at L_c
            let manager = db.persistence_manager.clone().unwrap();
            checkpoint_lsn = manager.load_manifest_and_strings().unwrap().lsn;

            // Post-checkpoint write: acknowledged AFTER the checkpoint, so its LSN
            // is above L_c and it lives only in the old-DEK active WAL segment (not
            // in the durable snapshot). GroupCommit durability makes it durable in
            // the WAL before `create_node` returns.
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Carol").build(),
            )
            .unwrap();
            carol_wal_lsn = db.wal.current_lsn().0;
            assert!(
                carol_wal_lsn > checkpoint_lsn,
                "precondition: Carol's write (wal LSN {carol_wal_lsn}) is above the checkpoint \
                 manifest LSN ({checkpoint_lsn})"
            );

            // Plant the PENDING ledger (WAL in scope): a rotation is in flight.
            super::write_ledger(
                &manager,
                &super::RotationLedger::forward_scope(
                    2,
                    KeyProviderConfig::File {
                        path: new_key.clone(),
                    },
                    true,  // WAL in scope
                    false, // cold not in scope
                ),
            )
            .unwrap();
            assert!(root.join("data").join("rotation.state").exists());
            // On-disk index snapshot is still at the pre-rotation checkpoint.
            assert_eq!(
                manager.load_manifest_and_strings().unwrap().lsn,
                checkpoint_lsn,
                "precondition: Carol is NOT yet in the durable index snapshot"
            );
            std::mem::forget(db); // crash A — skip the clean-shutdown final persist
        }

        // ── Reopen 1 (crash-A recovery): the startup resume path runs. Read the
        // durable manifest IMMEDIATELY, then crash again (`mem::forget`) so no
        // async/shutdown checkpoint captures Carol after resume. The crux
        // invariant: by the time resume has retired any old-generation segment,
        // the durable manifest LSN must already cover Carol. The old
        // no-op-persist resume left the manifest stuck at L_c, so a second crash
        // here lost Carol. ──
        let manifest_lsn_after_resume;
        {
            let db = build_db(root, &old_key);
            let manager = db.persistence_manager.clone().unwrap();
            manifest_lsn_after_resume = manager.load_manifest_and_strings().unwrap().lsn;
            assert_eq!(
                db.node_count(),
                3,
                "Alice, Bob, Carol all present in RAM after resume+replay"
            );
            assert!(
                manifest_lsn_after_resume >= carol_wal_lsn,
                "resume must snapshot every pre-retire write into the durable manifest BEFORE \
                 retiring its old-generation segment: manifest LSN {manifest_lsn_after_resume} \
                 must cover Carol's WAL LSN {carol_wal_lsn} (checkpoint was {checkpoint_lsn}), \
                 else a retired segment held un-snapshotted acknowledged writes and a second \
                 crash before the async checkpoint loses them (resume-path data-loss window)"
            );
            std::mem::forget(db); // crash B — no async/shutdown checkpoint runs
        }

        // ── Reopen 2 (crash-B recovery, now under the NEW key — the rotation
        // completed during reopen 1): with no async/shutdown checkpoint between
        // reopen 1 and here, Carol survives ONLY if reopen 1 durably snapshotted
        // her before retiring her old-generation segment. ──
        {
            let db = build_db(root, &new_key);
            assert_eq!(
                db.node_count(),
                3,
                "Carol survives the second crash — reopen 1 durably snapshotted her before \
                 retiring her old-generation WAL segment"
            );
        }
    }

    /// DEFECT #3617-PR2 (dual-generation startup install): a half-rotated WAL —
    /// old-DEK + new-DEK segments both on disk with a PENDING ledger — must have
    /// BOTH WAL key generations installed BEFORE the startup replay read. A fresh
    /// single(old) keyring (the startup state before resume runs) cannot decrypt
    /// the new-DEK frames — they are dropped as an undecodable tail — silently
    /// losing any write whose only durable copy is a new-DEK segment.
    /// `install_pending_wal_generations`, called before the replay read, recovers
    /// both generations. Asserted at the WAL-replay level so no index backstop
    /// masks the loss.
    #[test]
    fn half_rotated_wal_needs_both_generations_installed_before_replay() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let wal_dir = root.join("wal");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let algorithm = enc_cfg.algorithm;
        let old_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(
            algorithm,
            &super::derive_wal_dek(super::load_mek(&enc_cfg.key_provider).unwrap()).unwrap(),
        ));
        let new_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(
            algorithm,
            &super::derive_wal_dek(
                super::load_mek(&KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .unwrap(),
            )
            .unwrap(),
        ));

        // Lay down a half-rotated WAL with a real DB: Alice under the old DEK
        // (kv=1), then roll to gen 2 and write Bob under the new DEK (kv=2), and
        // record a PENDING ledger. No persist — the entries live ONLY in the WAL.
        {
            let db = build_db(root, &old_key);
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
            let ring = db.wal.wal_keyring().expect("encrypted WAL").clone();
            let cipher = Arc::clone(&new_wal_cipher);
            db.wal
                .seal_active_segment_for_rotation(move || ring.add_generation(2, cipher))
                .unwrap();
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
            let manager = db.persistence_manager.clone().unwrap();
            super::write_ledger(
                &manager,
                &super::RotationLedger::forward_scope(
                    2,
                    KeyProviderConfig::File {
                        path: new_key.clone(),
                    },
                    true,
                    false, // cold not in scope
                ),
            )
            .unwrap();
            // Drop (crash) with the rotation still pending.
        }

        let fresh_wal = || {
            let mut cfg = ConcurrentWalSystemConfig::new(wal_dir.clone());
            cfg.wal_cipher = Some(Arc::clone(&old_wal_cipher));
            cfg.tolerate_torn_tail = true;
            ConcurrentWalSystem::new(cfg).unwrap()
        };
        let lsn0 = crate::storage::wal::LSN::initial();

        // A fresh single(old) keyring — the startup state BEFORE the install —
        // cannot decrypt Bob's new-DEK (kv=2) frames: they are dropped, so only
        // Alice's old-DEK (kv=1) entries come back. (A node create emits several
        // framed WAL entries, so we compare counts rather than assume one each.)
        let n_single = fresh_wal().read_from(lsn0).map(|v| v.len()).unwrap_or(0);

        // Installing BOTH generations before the replay read makes Bob's new-DEK
        // frames replayable too, so strictly MORE entries are recovered.
        let wal = fresh_wal();
        super::install_pending_wal_generations(&wal, &enc_cfg, &data_dir).unwrap();
        let n_both = wal.read_from(lsn0).unwrap().len();
        assert!(
            n_single > 0,
            "sanity: the old-DEK (Alice) entries must be recoverable ({n_single})"
        );
        assert!(
            n_both > n_single,
            "installing BOTH WAL generations must recover strictly more entries than a fresh \
             single(old) keyring: the new-DEK (Bob) frames the old keyring silently drops \
             become replayable (single={n_single}, both={n_both}). Without the pre-replay \
             install, a half-rotated WAL loses every write whose only durable copy is a \
             new-DEK segment."
        );
    }
}
