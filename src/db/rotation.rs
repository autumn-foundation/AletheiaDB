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
//! * **Cancel (rollback) covers the index/checkpoint layer only.** The reverse
//!   pass drives only [`IndexKeyRotation`]; the full-MEK forward path's WAL /
//!   cold / subject-keyring passes have no counterpart. So a cancel is refused
//!   ([`RotationError::UnsupportedCancelWithRekeyedLayers`]) whenever the pending
//!   ledger records one of those layers as anything but `Skipped`, leaving the
//!   ledger untouched for a roll-FORWARD [`resume_pending_rotation`] instead —
//!   rather than reporting a successful rollback over a split-key database
//!   (Issue #3783).
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
use crate::db::encryption_state::{algorithm_from_token, algorithm_token};
use crate::encryption::audit::{AuditEvent, EncryptionAuditLogger};
use crate::encryption::cipher::Cipher;
use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
use crate::encryption::factory::Algorithm;
use crate::encryption::factory::create_cipher;
use crate::encryption::key_derivation::KeyDerivation;
use crate::encryption::wal_encryption::WalKeyring;
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
        // A refused cancel (Issue #3783) is an operator-precondition failure, not
        // a storage fault: the database is untouched and the remedy is a different
        // command (`--resume`). Surfaced as `FailedPrecondition` so it classifies
        // alongside the module's other actionable refusals (e.g.
        // `fail_if_pending_subject_keyring_without_crypto_shred`).
        refused @ RotationError::UnsupportedCancelWithRekeyedLayers { .. } => {
            Error::FailedPrecondition(refused.to_string())
        }
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
/// round-trip on this build (Issue #3587, relaxed by Issue #3620).
///
/// Since #3620 the `version=3` ledger serializes the FULL (non-secret)
/// [`KeyProviderConfig`] via serde, so passphrase/KMS/Vault sources DO
/// round-trip and resume — they are accepted here. The refusal is retained
/// **only** for a build compiled WITHOUT the `serde` feature, whose ledger can
/// still only persist the two-string File/Env reference: there, a secret-backed
/// source is refused fail-closed on its variant alone, BEFORE any
/// `load_mek`/network call, so an unreachable KMS/Vault surfaces the clean
/// refusal rather than a transport error. (Every gate/CI config — default and
/// `--all-features` — compiles serde in, so the relaxed path is the real one.)
fn refuse_unsupported_new_source(new_source: &KeyProviderConfig) -> Result<()> {
    match new_source {
        KeyProviderConfig::File { .. } | KeyProviderConfig::Env { .. } => Ok(()),
        KeyProviderConfig::PassphraseFile { .. }
        | KeyProviderConfig::Kms { .. }
        | KeyProviderConfig::Vault { .. } => {
            #[cfg(feature = "serde")]
            {
                // The v3 ledger round-trips the full config; resume re-derives
                // the MEK via `build_provider().get_mek()`. Accept it.
                Ok(())
            }
            #[cfg(not(feature = "serde"))]
            {
                let (provider_type, _) = new_source.describe();
                Err(StorageError::PersistenceError(format!(
                    "index key rotation to a {provider_type} key source requires the serde \
                     feature (the durable rotation ledger cannot round-trip it otherwise)"
                ))
                .into())
            }
        }
    }
}

/// Compute a MEK key-check value (KCV, Issue #3620 Approach C).
///
/// Returns the first 8 bytes of an HKDF-SHA256 subkey derived from the MEK under
/// a dedicated domain-separated context (`rotation-kcv`), rendered as lowercase
/// hex. This is a standard, non-secret KCV: HKDF is one-way, so it reveals
/// nothing about the MEK, yet two identical MEKs always yield the same value.
/// It uses only the codebase's existing [`KeyDerivation`] primitive — no new
/// crypto. The MEK never leaves `Zeroizing`.
///
/// # Threat model: operational, NOT tamper-evidence
///
/// The KCV is an **operational accidental-change detector**, not a security
/// control. The plaintext `rotation.state` ledger is **unauthenticated**: an
/// attacker who can write the file can also delete the `mek_kcv=` line, which
/// makes verification a no-op (fail-open **by design**, for back-compat with
/// legacy ledgers that carry no KCV). What actually backstops ledger integrity
/// is the layer ciphertext itself — a wrong key always fails the AEAD auth tag
/// on the WAL/index/cold data — so a tampered or deleted KCV can never cause a
/// wrong key to be *accepted*, only downgrade a precise KCV error to the same
/// loud AEAD failure that existed before this feature. The KCV's sole value is
/// turning that cryptic downstream AEAD failure into an actionable, up-front
/// "the source secret changed" message for the honest-operator case.
fn compute_mek_kcv(mek: &Zeroizing<[u8; 32]>) -> Result<String> {
    let subkey = KeyDerivation::new(mek.clone())
        .derive_dek("rotation-kcv")
        .map_err(|e| rotation_err(RotationError::KeyProvider(e.to_string())))?;
    Ok(subkey.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Verify a resumed rotation's re-derived MEK against the ledger's recorded KCV
/// (Issue #3620 Approach C). A no-op when the ledger carries no KCV (legacy
/// v1/v2 ledgers, non-serde builds, index-scope/cancel markers).
///
/// On mismatch — the source (passphrase/Vault/KMS value) yielded a DIFFERENT but
/// otherwise valid key than the rotation was started with — returns a precise,
/// actionable fail-closed error. Because this runs before any `clear_rotation_state`,
/// the ledger is retained: a later `open()` with the correct/restored secret
/// resumes losslessly.
fn verify_mek_kcv(ledger: &RotationLedger, mek: &Zeroizing<[u8; 32]>) -> Result<()> {
    verify_mek_kcv_value(ledger.mek_kcv.as_deref(), mek)
}

/// Constant-time compare an already-loaded MEK against an `expected` KCV string.
/// A no-op (`Ok(())`) when `expected` is `None`, so callers holding a view that
/// dropped its ledger (enable/disable resume) share the exact same fail-closed
/// semantics as [`verify_mek_kcv`] without reconstructing a `RotationLedger`.
fn verify_mek_kcv_value(expected: Option<&str>, mek: &Zeroizing<[u8; 32]>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual = compute_mek_kcv(mek)?;
    let matches =
        actual.len() == expected.len() && bool::from(actual.as_bytes().ct_eq(expected.as_bytes()));
    if !matches {
        return Err(StorageError::InconsistentState {
            reason: "rotation.state key-check value (KCV) mismatch: the key obtained from the \
                     recorded key source does not match the key this rotation was started with. \
                     The source's secret appears to have changed out-of-band (verify the \
                     passphrase / Vault value / KMS key is unchanged). The rotation ledger is \
                     retained; re-open with the original key to resume."
                .to_string(),
        }
        .into());
    }
    Ok(())
}

/// Re-derive the MEK from a resumed enable/disable ledger's recorded key
/// `source` and verify it against the ledger's stamped `mek_kcv` (Issue #3620),
/// BEFORE any wrap/unwrap pass runs. This closes a silent split-key corruption:
/// without it a source secret that changed out-of-band between the migration's
/// start and a crash-resume would re-key the still-pending files under the
/// *new* key while the already-processed files stay under the *old* key, with
/// NO error. Mirrors the forward-rotation [`load_mek_checked`] seam.
///
/// A no-op when `mek_kcv` is `None` (legacy/absent KCV) — it does NOT even load
/// the MEK in that case — so back-compat is exact. On mismatch it returns the
/// same structured, fail-closed [`StorageError::InconsistentState`] the forward
/// path uses; the caller leaves the ledger untouched (no clear, no wrap) so a
/// later `open()` with the correct secret resumes losslessly.
pub(crate) fn verify_resumed_source_kcv(
    source: &KeyProviderConfig,
    mek_kcv: Option<&str>,
) -> Result<()> {
    if mek_kcv.is_none() {
        return Ok(());
    }
    let mek = load_mek(source).map_err(rotation_err)?;
    verify_mek_kcv_value(mek_kcv, &mek)
}

/// Verify a resumed plaintext → encrypted **enable**'s configured algorithm still
/// matches the CONCRETE algorithm the ledger pinned at enable-start, and return the
/// concrete algorithm every resume wrap/keyring-build MUST run under (Issue #3708).
///
/// `pinned` is the ledger's `algorithm` field; `config_algorithm` is the algorithm
/// this `open()` resolved from its config (possibly `Auto`). The MEK-only `mek_kcv`
/// cannot detect an algorithm change, so without this a resume on a different CPU
/// class (`Auto` → AES on one host, ChaCha on another) or after an operator TOML
/// edit would wrap the still-plaintext files under a DIFFERENT algorithm than the
/// already-wrapped ones and pin the wrong algorithm as authority — a silent
/// split-algorithm brick.
///
/// Fail-closed contract (mirrors [`verify_resumed_source_kcv`]): on a mismatch, or
/// on a legacy `None` pin (an enable ledger that predates this field — its
/// already-wrapped files' algorithm is unknowable), it returns a structured
/// [`StorageError::InconsistentState`]. The caller leaves the ledger UNTOUCHED (no
/// clear, no wrap), so a later `open()` under the original algorithm/host resumes
/// losslessly. Logs/errors carry only the (non-secret) algorithm identifier, never
/// key material.
pub(crate) fn verify_resumed_enable_algorithm(
    pinned: Option<Algorithm>,
    config_algorithm: Algorithm,
) -> Result<Algorithm> {
    match pinned {
        Some(pinned) => {
            let now = config_algorithm.resolve();
            if now == pinned {
                Ok(pinned)
            } else {
                Err(StorageError::InconsistentState {
                    reason: format!(
                        "pending enable ledger pinned AEAD algorithm {} but this open() resolves \
                         {} (the CPU class or configured `encryption.algorithm` changed since the \
                         enable started); the ledger is RETAINED and the resume REFUSED. Reopen \
                         under the original algorithm/host so the migration completes under the \
                         same cipher every already-wrapped file uses.",
                        algorithm_token(pinned),
                        algorithm_token(now),
                    ),
                }
                .into())
            }
        }
        None => Err(StorageError::InconsistentState {
            reason: "pending enable ledger has no pinned AEAD algorithm (it was written by a \
                     pre-#3708 binary); refusing to resume under a possibly-different algorithm \
                     that could split the tier. The ledger is RETAINED. Complete the migration \
                     with the original binary/host, or clear the ledger only after confirming no \
                     at-rest file was wrapped."
                .to_string(),
        }
        .into()),
    }
}

/// Re-derive the MEK recorded in a ledger's `new_source` AND verify it against
/// the ledger's KCV (Issue #3620). Every resume seam re-derives the MEK through
/// this, so a wrong/changed source key is caught precisely and fail-closed at
/// the first re-derivation — with the ledger retained (loud and resumable).
/// Identical to a bare `load_mek(&ledger.new_source)` when the ledger has no KCV
/// (legacy v1/v2 ledgers, index-scope/cancel markers), preserving prior behavior.
fn load_mek_checked(ledger: &RotationLedger) -> Result<Zeroizing<[u8; 32]>> {
    let mek = load_mek(&ledger.new_source).map_err(rotation_err)?;
    verify_mek_kcv(ledger, &mek)?;
    Ok(mek)
}

/// Attach a freshly-computed KCV to a ledger from an already-loaded MEK. A no-op
/// on a non-serde build (the two-string v2 format has no KCV field).
fn attach_kcv_from_mek(ledger: &mut RotationLedger, mek: &Zeroizing<[u8; 32]>) -> Result<()> {
    #[cfg(feature = "serde")]
    {
        ledger.mek_kcv = Some(compute_mek_kcv(mek)?);
    }
    #[cfg(not(feature = "serde"))]
    {
        let _ = (&mut *ledger, mek);
    }
    Ok(())
}

/// Attach a KCV to a ledger by sourcing the MEK once from `source` (used by the
/// enable/disable engines, which hold the source rather than a loaded MEK). A
/// no-op on a non-serde build.
fn attach_kcv_from_source(ledger: &mut RotationLedger, source: &KeyProviderConfig) -> Result<()> {
    #[cfg(feature = "serde")]
    {
        let mek = load_mek(source).map_err(rotation_err)?;
        ledger.mek_kcv = Some(compute_mek_kcv(&mek)?);
    }
    #[cfg(not(feature = "serde"))]
    {
        let _ = (&mut *ledger, source);
    }
    Ok(())
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

/// HKDF component label for the crypto-shred subject-wrapping DEK (Slice 5). Must
/// match `crate::db::crypto_shred::api`'s `SUBJECT_WRAP_COMPONENT` so the rotation
/// driver re-derives the exact cipher the seal/unseal path uses.
#[cfg(feature = "audit-export")]
const SUBJECT_WRAP_COMPONENT: &str = "subject-wrap";

/// Derive the crypto-shred subject-wrapping DEK for a MEK using the shared HKDF
/// context (Slice 5). Yields HKDF info string `aletheiadb-subject-wrap-dek-v1`.
#[cfg(feature = "audit-export")]
fn derive_subject_wrap_dek(
    mek: Zeroizing<[u8; 32]>,
) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    KeyDerivation::new(mek)
        .derive_dek(SUBJECT_WRAP_COMPONENT)
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Test-only fault-injection seam (Slice 5): when set, the next
/// [`AletheiaDB::rotate_subject_keyring_layer`] returns an error BEFORE the
/// re-wrap pass, simulating a crash after the ledger recorded
/// `layer.subject_keyring=pending` but before the keyring was re-wrapped. The
/// flag is cleared on read so a subsequent resume completes normally.
#[cfg(all(test, feature = "audit-export"))]
pub(crate) static FAIL_BEFORE_SUBJECT_KEYRING_REWRAP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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
pub(crate) fn mark_wal_complete(manager: &IndexPersistenceManager) -> Result<()> {
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
pub(crate) fn mark_cold_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) = read_rotation_state(manager)?
        && ledger.cold != LayerStatus::Complete
    {
        ledger.cold = LayerStatus::Complete;
        write_ledger(manager, &ledger)?;
    }
    Ok(())
}

/// Rewrite the durable ledger flipping `layer.subject_keyring=complete` (Slice 5),
/// preserving every other field. A no-op when no ledger is present. Modeled on
/// [`mark_cold_complete`]: the completion is fsync'd (via `write_ledger`) before
/// the caller clears the ledger, so a crash between the re-wrap's durable save
/// and this mark re-runs the idempotent re-wrap pass on the next resume.
#[cfg(feature = "audit-export")]
pub(crate) fn mark_subject_keyring_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) =
        read_rotation_state(manager)?.filter(|l| l.subject_keyring != LayerStatus::Complete)
    {
        ledger.subject_keyring = LayerStatus::Complete;
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
    /// # Arbitrary version transitions (Issue #3680)
    ///
    /// The generation to roll BACK to is derived from the pending ledger
    /// ([`RotationLedger::rotation_old_version`]), not assumed to be the base
    /// version. Cancelling the *second* (or n-th) rotation of a database — a
    /// `v2 → v3` pass, say — therefore rolls back to `v2`, the same authority
    /// [`resolve_provisioned_key_version`] uses to provision the startup keyring.
    /// Previously this was hard-coded to the base version, so any rotation other
    /// than the first `v1 → v2` cancelled into a keyring holding the wrong
    /// generation number: the un-migrated old-key files (stamped `v2`) matched no
    /// live generation and the reverse pass failed to decrypt them.
    ///
    /// An `enable` / `disable` migration ledger is NOT a rotation and is refused
    /// with [`RotationError::NotInProgress`] — its `new_version` does not mean
    /// `old + 1` (see [`RotationDirection::Disable`]), so driving it through the
    /// reverse rotation pass would target a bogus generation.
    ///
    /// # Scope: index/checkpoint ONLY — other re-keyed layers are refused (#3783)
    ///
    /// This drives exactly one engine, the index/checkpoint
    /// [`IndexKeyRotation`] reverse pass. A full-MEK forward rotation (#3617) also
    /// re-keys the WAL, the cold (redb) tier, and the crypto-shred subject
    /// keyring, and **none of those has a reverse pass**. So when the pending
    /// ledger records any of `layer.wal` / `layer.cold` /
    /// `layer.subject_keyring` as something other than `Skipped`, the cancel is
    /// **refused** with
    /// [`RotationError::UnsupportedCancelWithRekeyedLayers`] naming those layers,
    /// before anything is touched — the pending forward ledger is left
    /// byte-identical so the operator can roll FORWARD with
    /// [`resume_pending_index_rotation`](Self::resume_pending_index_rotation)
    /// instead (idempotent across every layer), keeping the old key available
    /// until it completes.
    ///
    /// Previously the cancel ran the index reverse pass regardless, cleared the
    /// ledger, and returned `Ok`, telling the operator the new key was never
    /// adopted while the WAL / cold / subject-keyring bytes were still on it —
    /// discarding the new key then made them permanently undecryptable. An
    /// index-only rotation (every non-index layer `Skipped`) is exactly what the
    /// reverse pass does cover, so it still cancels normally.
    ///
    /// # Errors
    ///
    /// * no rotation is pending (including when only an enable/disable migration
    ///   ledger is present);
    /// * the pending rotation already re-keyed a layer this reverse pass cannot
    ///   roll back ([`RotationError::UnsupportedCancelWithRekeyedLayers`]);
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
        // An enable/disable migration ledger rides the same file but is NOT a
        // rotation (its `new_version` is not `old + 1`), so it has no old
        // generation to roll back to. Refuse rather than cancel it as a rotation.
        if matches!(
            pending.direction,
            RotationDirection::Enable | RotationDirection::Disable
        ) {
            return Err(rotation_err(RotationError::NotInProgress));
        }

        // Issue #3783: the reverse pass below drives ONLY the index/checkpoint
        // engine. If the interrupted forward rotation already moved the WAL, the
        // cold tier, or the crypto-shred subject keyring onto the new key, rolling
        // the index back would leave the database split-key while reporting
        // success. Refuse LOUDLY instead — before any keyring generation is
        // installed and before the cancel marker is published, so a refused cancel
        // leaves the pending forward ledger byte-identical for `--resume`.
        let blocked = pending.unrollbackable_layers();
        if !blocked.is_empty() {
            let err = rotation_err(RotationError::UnsupportedCancelWithRekeyedLayers {
                layers: blocked.join(", "),
            });
            self.log_rotation_failure(&manager, &err);
            return Err(err);
        }

        let started = Instant::now();
        let keyring = manager
            .keyring()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        // The keyring's current generation is the one written by the interrupted
        // forward pass; the *old* generation to roll back to is `enc_cfg`.
        let old_dek = derive_index_dek(load_mek(&enc_cfg.key_provider).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let old_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &old_dek));
        // Issue #3680: the generation being rolled back TO comes from the ledger,
        // never a hard-coded base version — a `vN → vN+1` rotation cancels back to
        // `vN` for any N, not just `v1`.
        let old_version = pending.rotation_old_version();

        let new_dek = derive_index_dek(load_mek(&pending.new_source).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));
        // Ensure BOTH generations are live so the reverse pass can read new-key
        // files and re-stamp them old (add_generation replaces if present).
        keyring.add_generation(old_version, Arc::clone(&old_cipher));
        keyring.add_generation(pending.new_version, Arc::clone(&new_cipher));

        // Publish the cancel marker durably BEFORE rolling anything back. Derived
        // FROM the pending ledger (Issue #3783) so the per-layer statuses and the
        // #3620 `mek_kcv` survive the direction flip instead of being flattened to
        // an index-scope marker.
        write_ledger(&manager, &RotationLedger::cancel_scope(&pending))?;

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
        // Subject-keyring layer (Slice 5): the LAST layer to settle — finish any
        // pending re-wrap here (idempotent; skips already-rewrapped entries) and,
        // if it was the only remaining pending layer, clear the ledger. Runs AFTER
        // the cold finalize. A no-op when the layer is not in flight.
        #[cfg(feature = "audit-export")]
        if report.is_some() {
            finalize_resumed_subject_keyring_rotation(&manager, &enc_cfg, &self.crypto_shred)?;
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
        // Subject-keyring layer (Slice 5): in scope when encryption is configured
        // AND the crypto-shred keyring holds a wrapped DEK stamped below the target
        // version (a new MEK re-derives the `subject-wrap` cipher, so those DEKs
        // must be re-wrapped). No subjects, or all already at the target, → Skipped.
        // Gated on `audit-export` (the crypto-shred module's feature); without it
        // there is no subject keyring, so the layer is never in scope.
        #[cfg(feature = "audit-export")]
        let subject_keyring_in_scope = self.encryption_manager.is_some()
            && self
                .crypto_shred
                .keyring_has_stale_wrapped_entries(new_version);
        #[cfg(not(feature = "audit-export"))]
        let subject_keyring_in_scope = false;

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

        let mut forward_ledger = RotationLedger::forward_scope(
            new_version,
            new_key_source.clone(),
            wal_in_scope,
            cold_in_scope,
            subject_keyring_in_scope,
        );
        // Stamp the KCV of the new MEK (already loaded above) so a crash-resume
        // can detect a wrong/changed key at the source precisely (Issue #3620).
        attach_kcv_from_mek(&mut forward_ledger, &new_mek)?;
        write_ledger(manager, &forward_ledger)?;
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

        // Subject-keyring layer (Slice 5): re-wrap every per-subject wrapped DEK
        // from the old MEK's `subject-wrap` cipher to the new one. Runs at the TAIL
        // — after index/WAL/cold — so it settles last, immediately before the
        // ledger is cleared. Both MEKs are still in scope here. A no-op when the
        // keyring holds no stale-generation entries (layer recorded `Skipped`).
        #[cfg(feature = "audit-export")]
        if subject_keyring_in_scope {
            self.rotate_subject_keyring_layer(manager, enc_cfg, &old_mek, &new_mek, new_version)?;
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

    /// Re-wrap the crypto-shred subject keyring to the new MEK's `subject-wrap`
    /// cipher (Slice 5), then flip `layer.subject_keyring=complete`.
    ///
    /// A full-MEK rotation re-derives every per-component DEK, including the
    /// `subject-wrap` DEK used to wrap the random per-subject DEKs in
    /// `subject_keyring.dat`. Those wrapped DEKs are the ONLY durable copies of an
    /// active subject's key, so they must be re-wrapped under the new MEK before
    /// the operator drops the old key — otherwise the subjects (and every value
    /// sealed under them) become undecryptable. Derives the old + new
    /// subject-wrap ciphers from the two MEKs (already loaded in `run_rotation`),
    /// runs the idempotent [`CryptoShredState::rewrap_keyring`] pass (which is a
    /// no-op for erased subjects and for entries already at the target version),
    /// and marks the ledger layer complete.
    ///
    /// # Operational contract (keep the OLD MEK until this completes)
    /// Mirrors the cold layer's contract: while `layer.subject_keyring` is
    /// `Pending`, the OLD MEK must remain available (the resume derives the old
    /// subject-wrap cipher from it to unwrap the still-old-generation DEKs). A
    /// reopen under the NEW key alone mid-pass surfaces a loud AEAD failure, never
    /// silent wrong data; reopening under the OLD key lets the resume finish.
    #[cfg(feature = "audit-export")]
    fn rotate_subject_keyring_layer(
        &self,
        manager: &IndexPersistenceManager,
        enc_cfg: &EncryptionConfig,
        old_mek: &Zeroizing<[u8; 32]>,
        new_mek: &Zeroizing<[u8; 32]>,
        new_version: u32,
    ) -> Result<()> {
        #[cfg(test)]
        if FAIL_BEFORE_SUBJECT_KEYRING_REWRAP.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(StorageError::InconsistentState {
                reason: "injected fault before subject-keyring re-wrap (test)".to_string(),
            }
            .into());
        }

        let old_dek = derive_subject_wrap_dek(old_mek.clone()).map_err(rotation_err)?;
        let new_dek = derive_subject_wrap_dek(new_mek.clone()).map_err(rotation_err)?;
        let old_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &old_dek));
        let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));

        self.crypto_shred
            .rewrap_keyring(old_cipher.as_ref(), &new_cipher, new_version)
            .map_err(|e| rotation_err(RotationError::KeyProvider(e.to_string())))?;
        mark_subject_keyring_complete(manager)?;
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
    /// A plaintext → encrypted **enable** migration (Issue #3616 PR3). Recorded
    /// on the same cross-layer ledger as a rotation, but distinguished by this
    /// direction so the rotation resume paths (which assume an already-encrypted
    /// old→new generation, gated on `encryption.enabled`) never touch an enable
    /// ledger — the enable engine's own `resume_pending_enable` drives it.
    Enable,
    /// An encrypted → plaintext **disable** migration (Issue #3616 PR4) — the
    /// mirror of [`Enable`](RotationDirection::Enable). Recorded on the same
    /// cross-layer ledger, but distinguished by this direction so the rotation
    /// resume paths (which assume an already-encrypted old→new generation) never
    /// touch a disable ledger — the disable engine's own resume drives it. On a
    /// disable ledger `new_version` / `new_source` name the CURRENT (to-be-retired)
    /// key coordinates, i.e. the key a resumed disable uses to DECRYPT each layer.
    Disable,
}

impl RotationDirection {
    fn as_str(self) -> &'static str {
        match self {
            RotationDirection::Forward => "forward",
            RotationDirection::Cancel => "cancel",
            RotationDirection::Enable => "enable",
            RotationDirection::Disable => "disable",
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
    /// The plaintext → encrypted **enable** WAL-cleanup step (Issue #3616 PR3):
    /// after the WAL is rolled to encrypted AND the pre-enable state is durably in
    /// the encrypted (`AEIX`) index snapshot, the sealed pre-enable *plaintext*
    /// (v13) WAL segments are retired so no cleartext survives at rest. A distinct
    /// per-field breadcrumb (never positional) so a crash mid-retire re-runs it.
    /// Only ever `Pending` for an `enable` ledger with the index layer in scope
    /// (there must be a durable snapshot holding the pre-enable state before any
    /// plaintext WAL is deleted); `Skipped` for rotations and for an enable with no
    /// index snapshot.
    wal_retire: LayerStatus,
    /// MEK key-check value (Issue #3620, Approach C): a short, non-secret
    /// one-way check value of the NEW/current MEK this rotation targets.
    ///
    /// Computed as the first 8 bytes of an HKDF subkey derived from the MEK
    /// (`compute_mek_kcv`) rendered as lowercase hex. It reveals nothing about
    /// the key (HKDF is one-way) yet lets a crash-resume detect a *wrong-key*
    /// source (a passphrase/Vault/KMS value that changed out-of-band between
    /// rotation start and resume) and surface a precise, actionable error
    /// instead of a cryptic downstream AEAD failure.
    ///
    /// Persisted only in the `version=3` (serde) ledger; `None` for legacy v1/v2
    /// ledgers, for the two-string (non-serde) writer, and for the index-scope
    /// (`write_rotation_state`) / cancel markers that carry no loaded MEK. A
    /// `None` KCV makes resume verification a no-op, preserving prior behavior.
    mek_kcv: Option<String>,
    /// The crypto-shred subject-keyring re-wrap step (Slice 5): after a full-MEK
    /// rotation re-keys the index/WAL/cold layers, every per-subject wrapped DEK
    /// in `subject_keyring.dat` (still wrapped under the OLD MEK's `subject-wrap`
    /// cipher) is re-wrapped under the NEW MEK's `subject-wrap` cipher. Recorded
    /// `Pending` only for a forward rotation whose keyring holds stale-generation
    /// wrapped DEKs; `Skipped` otherwise (no subjects, all already at the target
    /// version, or for enable ledgers). A distinct per-field breadcrumb (mirroring
    /// `wal_retire` / `cold`) so a crash mid-re-wrap re-runs it. Absent in any
    /// pre-Slice-5 ledger, defaulting to `Skipped` on read.
    subject_keyring: LayerStatus,
    /// The CONCRETE (resolved) AEAD algorithm a plaintext → encrypted **enable**
    /// migration is wrapping every at-rest layer under (Issue #3708). Stamped at
    /// enable-start as `algorithm.resolve()` so `Auto` is NEVER persisted here:
    /// pinning the concrete variant lets an interrupted-enable resume detect a
    /// host/config algorithm change (a cross-CPU-class reopen where `Auto`
    /// resolves differently, or an operator TOML edit) and refuse BEFORE wrapping
    /// the remaining files under a *different* algorithm than the already-wrapped
    /// ones — the split-algorithm corruption the MEK-only `mek_kcv` cannot catch.
    ///
    /// `Some` only for a `direction=enable` ledger written by a post-#3708 binary;
    /// `None` for every rotation/disable ledger, and for a legacy enable ledger
    /// written before this field existed. A resume reading `None` on an enable
    /// ledger fails **closed** (`verify_resumed_enable_algorithm`): it cannot
    /// prove the resume host's algorithm matches what the already-wrapped files
    /// used, so it retains the ledger and refuses rather than risk a split.
    algorithm: Option<Algorithm>,
}

impl RotationLedger {
    /// The OLD (pre-rotation) key generation a `forward` / `cancel` ledger is
    /// rotating away from — the single authority both the cancel driver
    /// ([`AletheiaDB::cancel_pending_rotation`]) and startup keyring provisioning
    /// ([`resolve_provisioned_key_version`]) resolve it from (Issue #3680).
    ///
    /// A forward rotation always targets `new_version = old_version + 1`
    /// (`run_rotation`), and the currently CONFIGURED key still corresponds to the
    /// OLD generation while a rotation is pending (the provider switch happens
    /// only after a rotation COMPLETES). So the old generation is `new_version - 1`
    /// for ANY rotation of the database, not only the first `v1 → v2` one. Floored
    /// at the base version so a malformed/legacy `target_version=0` ledger can
    /// never resolve to a nonexistent generation.
    ///
    /// Only meaningful for `Forward` / `Cancel`. An `Enable` ledger records
    /// `new_version == BASE` (no prior generation) and a `Disable` ledger records
    /// the CURRENT to-be-retired version verbatim (see
    /// [`RotationDirection::Disable`]) — neither is `old + 1`, so callers must
    /// screen those directions out first (both do).
    fn rotation_old_version(&self) -> u32 {
        debug_assert!(
            matches!(
                self.direction,
                RotationDirection::Forward | RotationDirection::Cancel
            ),
            "rotation_old_version is only defined for forward/cancel ledgers",
        );
        self.new_version
            .saturating_sub(1)
            .max(ENC_INDEX_KEY_VERSION_V1)
    }

    /// The non-index layers whose recorded status BLOCKS a cancel (reverse) pass,
    /// rendered as `layer=status` pairs (Issue #3783).
    ///
    /// The cancel driver reverses only the index/checkpoint tree. A full-MEK
    /// forward rotation (#3617) also re-keys the WAL, the cold (redb) tier, and the
    /// crypto-shred subject keyring, and there is no reverse pass for any of them —
    /// so a cancel from a ledger recording ANY of those as `Pending` or `Complete`
    /// would roll the index back to the old key while leaving those layers on the
    /// new one (a split-key database) and then report success.
    ///
    /// `Pending` blocks just as `Complete` does, deliberately: the ledger cannot
    /// distinguish "the layer's pass never started" from "it was interrupted
    /// half-way", and a half-rolled WAL has already had its old-generation segments
    /// retired. Fail closed on the ambiguity — the operator rolls FORWARD instead
    /// (`--resume`), which IS symmetric and idempotent across every layer.
    ///
    /// Empty for an index-scope ledger (every non-index layer `Skipped`), which is
    /// exactly the case the reverse pass does cover, so cancelling stays available
    /// for an index-only rotation. `wal_retire` is deliberately absent: it is an
    /// enable/disable cleanup step, always `Skipped` on a rotation ledger, and
    /// names no key generation to roll back.
    fn unrollbackable_layers(&self) -> Vec<String> {
        [
            ("wal", self.wal),
            ("cold", self.cold),
            ("subject_keyring", self.subject_keyring),
        ]
        .into_iter()
        .filter(|(_, status)| !matches!(status, LayerStatus::Skipped))
        .map(|(name, status)| format!("{name}={}", status.as_str()))
        .collect()
    }

    /// The reverse (cancel) plan derived FROM the pending forward ledger being
    /// cancelled (Issue #3783).
    ///
    /// The index/checkpoint layers are reset to `Pending` because the reverse pass
    /// still has to run over them. Every OTHER per-layer status — and the `mek_kcv`
    /// — is **preserved verbatim** rather than flattened to `Skipped`/`None` the way
    /// [`index_scope`](RotationLedger::index_scope) does:
    ///
    /// * the non-index statuses are the only durable record of which layers the
    ///   interrupted forward pass had already moved onto the new key, so flattening
    ///   them would make an interrupted cancel unable to tell what still needs
    ///   undoing (and would silently erase the evidence the
    ///   [`unrollbackable_layers`](RotationLedger::unrollbackable_layers) guard
    ///   reads);
    /// * dropping `mek_kcv` would lose the #3620 wrong-key detection across a
    ///   cancel, downgrading a precise "the source secret changed" error back to a
    ///   cryptic downstream AEAD failure.
    ///
    /// In practice the guard means a cancel marker written by this binary always
    /// carries `Skipped` non-index layers — but recording the truth is what lets a
    /// future symmetric reverse pass (or a diagnosing operator) reconstruct the
    /// state, and it keeps the marker honest if the guard is ever relaxed.
    fn cancel_scope(pending: &RotationLedger) -> Self {
        Self {
            direction: RotationDirection::Cancel,
            new_version: pending.new_version,
            new_source: pending.new_source.clone(),
            // The reverse index/checkpoint pass has not run yet.
            index: LayerStatus::Pending,
            checkpoint: LayerStatus::Pending,
            // PRESERVE — never flatten: what the forward pass already re-keyed.
            wal: pending.wal,
            cold: pending.cold,
            wal_retire: pending.wal_retire,
            subject_keyring: pending.subject_keyring,
            // PRESERVE the key-check value so #3620 wrong-key detection survives.
            mek_kcv: pending.mek_kcv.clone(),
            // Enable-only (`None` on any rotation ledger); carried through so a
            // future direction that does pin one cannot lose it here.
            algorithm: pending.algorithm,
        }
    }

    /// The default PR1 cross-layer plan: index and checkpoint re-key through
    /// the index engine (checkpoints ride the index DEK), while WAL and cold
    /// are out of scope and recorded `Skipped`.
    ///
    /// Test-only since Issue #3783: the cancel driver was its last production
    /// caller and now derives its marker from the pending ledger
    /// ([`cancel_scope`](RotationLedger::cancel_scope)) instead of flattening the
    /// per-layer statuses through this constructor. Retained because the ledger
    /// tests use it to plant index-scope forward/cancel/enable/disable ledgers.
    #[cfg(test)]
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
            wal_retire: LayerStatus::Skipped,
            mek_kcv: None,
            // The index-only PR1 path never re-wraps the subject keyring.
            subject_keyring: LayerStatus::Skipped,
            // Algorithm pinning is an ENABLE-only concern (Issue #3708).
            algorithm: None,
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
        subject_keyring_in_scope: bool,
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
            // A key rotation never retires pre-enable plaintext WAL (there is none).
            wal_retire: LayerStatus::Skipped,
            mek_kcv: None,
            // Slice 5: re-wrap the subject keyring when it holds stale-generation
            // wrapped DEKs (else nothing to do — record Skipped).
            subject_keyring: scope(subject_keyring_in_scope),
            // Algorithm pinning is an ENABLE-only concern (Issue #3708).
            algorithm: None,
        }
    }

    /// The plaintext → encrypted **enable** plan (Issue #3616 PR3).
    ///
    /// The WAL is ALWAYS `Pending` (an enable's whole point is to encrypt the
    /// live WAL). Index / checkpoint / cold are `Pending` only when there are
    /// bytes of that layer to wrap (`*_in_scope`), else `Skipped` — the WAL-first
    /// engine records index/checkpoint/cold `Skipped` until their bare→encrypted
    /// wrap passes land (a follow-up worker flips them to `Pending`). Callers and
    /// the resumer MUST drive off each per-field [`LayerStatus`], never a count of
    /// layers, so adding a future layer cannot silently skip migration or resume.
    fn enable_scope(
        new_version: u32,
        new_source: KeyProviderConfig,
        index_in_scope: bool,
        cold_in_scope: bool,
        algorithm: Algorithm,
    ) -> Self {
        let scope = |in_scope: bool| {
            if in_scope {
                LayerStatus::Pending
            } else {
                LayerStatus::Skipped
            }
        };
        Self {
            direction: RotationDirection::Enable,
            new_version,
            new_source,
            index: scope(index_in_scope),
            checkpoint: scope(index_in_scope),
            wal: LayerStatus::Pending,
            cold: scope(cold_in_scope),
            // Retire the pre-enable plaintext WAL only when there is an index
            // snapshot to durably hold the pre-enable state first (index in scope).
            // With no snapshot, retiring plaintext WAL could lose data, so Skip it.
            wal_retire: scope(index_in_scope),
            mek_kcv: None,
            // Enabling encryption from plaintext establishes the FIRST MEK; there
            // is no prior subject-wrap generation to re-wrap from, so the
            // subject-keyring re-wrap step never runs on an enable.
            subject_keyring: LayerStatus::Skipped,
            // Pin the CONCRETE algorithm (Issue #3708). `.resolve()` here means
            // `Auto` is never persisted, so a cross-host/config resume can detect a
            // divergence instead of silently wrapping under a different algorithm.
            algorithm: Some(algorithm.resolve()),
        }
    }

    /// The encrypted → plaintext **disable** plan (Issue #3616 PR4) — the exact
    /// mirror of [`enable_scope`](RotationLedger::enable_scope).
    ///
    /// The WAL is ALWAYS `Pending` (a disable's whole point is to strip the live
    /// WAL's encryption). Index / checkpoint / cold are `Pending` only when there
    /// are encrypted bytes of that layer to UNWRAP back to plaintext
    /// (`*_in_scope`), else `Skipped`. `new_version` / `new_source` carry the
    /// CURRENT (to-be-retired) key coordinates so a resumed disable can rebuild the
    /// DECRYPT cipher from the same recorded source.
    ///
    /// Callers and the resumer MUST drive off each per-field [`LayerStatus`] BY
    /// NAME — never a positional index or a count of layers — so a future layer
    /// added the same by-name way (e.g. the GDPR `subject_keyring` layer) composes
    /// without editing the disable path.
    //
    // Consumed in non-test builds by the disable-engine driver
    // (`write_disable_ledger` → `disable_encryption` / `resume_pending_disable`,
    // Issue #3616 PR4).
    fn disable_scope(
        current_version: u32,
        current_source: KeyProviderConfig,
        index_in_scope: bool,
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
            direction: RotationDirection::Disable,
            new_version: current_version,
            new_source: current_source,
            index: scope(index_in_scope),
            checkpoint: scope(index_in_scope),
            wal: LayerStatus::Pending,
            cold: scope(cold_in_scope),
            // Retire the pre-disable ENCRYPTED WAL only when there is an index
            // snapshot to durably hold the decrypted state first (index in scope) —
            // the mirror of the enable retire guard. With no snapshot, retiring the
            // encrypted WAL could lose data, so Skip it.
            wal_retire: scope(index_in_scope),
            mek_kcv: None,
            // Disabling encryption RETIRES the MEK entirely; there is no new
            // subject-wrap generation to re-wrap the keyring into (the mirror of the
            // enable case), so the subject-keyring re-wrap step never runs on a
            // disable — always `Skipped`, never `Pending`.
            subject_keyring: LayerStatus::Skipped,
            // Algorithm pinning is an ENABLE-only concern (Issue #3708); a disable
            // strips encryption, so there is no target algorithm to pin.
            algorithm: None,
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
/// cold skipped). Signature-compatible with the #488 breadcrumb writer; the
/// on-disk format is now `version=3` (the serde full-config writer; `version=2`
/// only on a non-serde build).
///
/// Test-only since Issue #3783 — see
/// [`RotationLedger::index_scope`] for why the cancel driver stopped using it.
#[cfg(test)]
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

/// Durably write a cross-layer rotation ledger.
///
/// **Format history:** `version=1` (#488, index-only), `version=2` (#3617,
/// cross-layer, two-string `new_source_kind`/`new_source_value` reference —
/// File/Env only), `version=3` (#3620, the FULL non-secret [`KeyProviderConfig`]
/// serialized with serde_json on a single `new_source_json=` line, plus an
/// optional `mek_kcv=` key-check value). v3 is what a serde-enabled build emits;
/// it round-trips passphrase/KMS/Vault sources that the two-string form could
/// not. No key material is ever written — the config carries file paths, env-var
/// NAMES, a KMS-wrapped (CMK-useless) blob, or a Vault address, never a secret
/// (verified by `ledger_v3_never_contains_plaintext_secret`).
///
/// A build compiled WITHOUT `serde` falls back to the `version=2` writer and
/// still refuses secret-backed sources, exactly preserving prior behavior.
fn write_ledger(manager: &IndexPersistenceManager, ledger: &RotationLedger) -> Result<()> {
    let base = manager.base_path();
    std::fs::create_dir_all(base)
        .map_err(|e| StorageError::io_error(format!("Failed to create data dir: {e}")))?;
    let body = serialize_ledger(ledger)?;
    write_durable(&rotation_state_path(manager), body.as_bytes())
}

/// Serialize a ledger's on-disk body. `version=3` under serde (full config +
/// optional KCV); `version=2` two-string form otherwise.
///
/// Note: a non-UTF-8 key-source path (e.g. a `File { path }` with invalid UTF-8)
/// now fails **closed** here — `serde_json` refuses to serialize it and the
/// write errors — rather than being silently mangled through the old v2
/// writer's `to_string_lossy()`. Failing the rotation write is strictly safer:
/// a lossily-rewritten path would round-trip to the WRONG file on resume.
#[cfg(feature = "serde")]
fn serialize_ledger(ledger: &RotationLedger) -> Result<String> {
    // Compact serde_json is single-line (string values escape newlines as `\n`
    // literals, never a raw LF), so it is a newline-safe token in the line-based
    // ledger. `split_once('=')` in the reader splits only the FIRST `=`, so any
    // `=` inside the JSON (e.g. base64 `=` padding in a KMS blob) is preserved.
    let new_source_json = serde_json::to_string(&ledger.new_source).map_err(|e| {
        StorageError::PersistenceError(format!("failed to serialize rotation new_source: {e}"))
    })?;
    debug_assert!(
        !new_source_json.contains('\n'),
        "compact serde_json must be single-line"
    );
    let mut body = format!(
        "version=3\ndirection={}\ntarget_version={}\nnew_source_json={new_source_json}\n",
        ledger.direction.as_str(),
        ledger.new_version,
    );
    if let Some(kcv) = &ledger.mek_kcv {
        body.push_str(&format!("mek_kcv={kcv}\n"));
    }
    // The enable-pinned concrete AEAD algorithm (Issue #3708). Additive line,
    // present only for a `direction=enable` ledger; absent for rotation/disable.
    if let Some(algorithm) = ledger.algorithm {
        body.push_str(&format!("algorithm={}\n", algorithm_token(algorithm)));
    }
    body.push_str(&format!(
        "layer.index={}\nlayer.checkpoint={}\nlayer.wal={}\nlayer.cold={}\nlayer.wal_retire={}\nlayer.subject_keyring={}\n",
        ledger.index.as_str(),
        ledger.checkpoint.as_str(),
        ledger.wal.as_str(),
        ledger.cold.as_str(),
        ledger.wal_retire.as_str(),
        ledger.subject_keyring.as_str(),
    ));
    Ok(body)
}

/// Non-serde fallback: the legacy `version=2` two-string writer. File/Env only;
/// secret-backed sources are refused (they cannot round-trip without serde).
#[cfg(not(feature = "serde"))]
fn serialize_ledger(ledger: &RotationLedger) -> Result<String> {
    let (kind, value) = match &ledger.new_source {
        KeyProviderConfig::File { path } => ("file", path.to_string_lossy().into_owned()),
        KeyProviderConfig::Env { variable } => ("env", variable.clone()),
        KeyProviderConfig::PassphraseFile { .. }
        | KeyProviderConfig::Kms { .. }
        | KeyProviderConfig::Vault { .. } => {
            let (provider_type, _) = ledger.new_source.describe();
            return Err(StorageError::PersistenceError(format!(
                "key rotation to a {provider_type} key source requires the serde feature"
            ))
            .into());
        }
    };
    // The enable-pinned concrete AEAD algorithm (Issue #3708). Additive line,
    // present only for a `direction=enable` ledger; absent for rotation/disable.
    let algorithm_line = match ledger.algorithm {
        Some(algorithm) => format!("algorithm={}\n", algorithm_token(algorithm)),
        None => String::new(),
    };
    Ok(format!(
        "version=2\ndirection={}\ntarget_version={}\nnew_source_kind={kind}\nnew_source_value={value}\n{algorithm_line}layer.index={}\nlayer.checkpoint={}\nlayer.wal={}\nlayer.cold={}\nlayer.wal_retire={}\nlayer.subject_keyring={}\n",
        ledger.direction.as_str(),
        ledger.new_version,
        ledger.index.as_str(),
        ledger.checkpoint.as_str(),
        ledger.wal.as_str(),
        ledger.cold.as_str(),
        ledger.wal_retire.as_str(),
        ledger.subject_keyring.as_str(),
    ))
}

/// Durable, ordered file write: temp file → `sync_all` → atomic rename → fsync
/// of the parent directory.
///
/// Moved to [`crate::db::durable_write`] (an always-compiled module) so the
/// small set of durable-breadcrumb writers can share it without depending on the
/// disk/encryption-only rotation engine. Re-exported here so the historical
/// `crate::db::rotation::write_durable` path (and this module's internal ledger
/// writers) keep working unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use crate::db::durable_write::write_durable;

pub(crate) fn clear_rotation_state(manager: &IndexPersistenceManager) {
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
    // v3 (#3620): full serde_json config + optional KCV.
    let mut new_source_json = None;
    let mut mek_kcv = None;
    let mut algorithm_line = None;
    let mut layer_index = None;
    let mut layer_checkpoint = None;
    let mut layer_wal = None;
    let mut layer_cold = None;
    let mut layer_wal_retire = None;
    let mut layer_subject_keyring = None;
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
                    "enable" => RotationDirection::Enable,
                    "disable" => RotationDirection::Disable,
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
            // v3 (#3620): the value is the remainder of the line after the first
            // `=`, i.e. the full compact JSON (internal `=` preserved).
            "new_source_json" => new_source_json = Some(v.to_string()),
            "mek_kcv" => mek_kcv = Some(v.to_string()),
            // Issue #3708: the enable-pinned concrete AEAD algorithm token.
            "algorithm" => algorithm_line = Some(v.to_string()),
            "layer.index" => layer_index = Some(parse_layer(v, "layer.index")?),
            "layer.checkpoint" => layer_checkpoint = Some(parse_layer(v, "layer.checkpoint")?),
            "layer.wal" => layer_wal = Some(parse_layer(v, "layer.wal")?),
            "layer.cold" => layer_cold = Some(parse_layer(v, "layer.cold")?),
            "layer.wal_retire" => layer_wal_retire = Some(parse_layer(v, "layer.wal_retire")?),
            "layer.subject_keyring" => {
                layer_subject_keyring = Some(parse_layer(v, "layer.subject_keyring")?)
            }
            _ => {}
        }
    }

    // Fail closed on an absent or unknown format version. v1 (index-only), v2
    // (cross-layer, two-string source) and v3 (#3620, serde source + KCV) are the
    // only supported shapes.
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
        // v2 / v3: an absent `layer.*` line defaults to Pending (forward-safe).
        // v3 differs from v2 ONLY in how `new_source` is encoded (full serde JSON
        // + optional KCV) — the per-layer semantics are identical, so they share
        // these defaults.
        2 | 3 => (
            LayerStatus::Pending,
            LayerStatus::Pending,
            LayerStatus::Pending,
            LayerStatus::Pending,
        ),
        other => return Err(corrupt(&format!("unsupported ledger version {other}"))),
    };

    // Issue #3708: a present `algorithm=` token must parse to a known concrete
    // algorithm; an unrecognized token is corrupt (fail-closed, never a silent
    // wrong-cipher resume). Absent → `None` (a legacy enable ledger with no pin, or
    // any rotation/disable ledger); the resume verifier fails closed on `None`.
    let algorithm = match algorithm_line {
        Some(token) => Some(
            algorithm_from_token(&token)
                .ok_or_else(|| corrupt(&format!("unknown algorithm {token:?}")))?,
        ),
        None => None,
    };

    let new_version = target_version.ok_or_else(|| corrupt("missing target_version"))?;
    // Reconstruct the new-key SOURCE. Prefer the v3 full-config JSON when present
    // (round-trips passphrase/KMS/Vault); fall back to the v1/v2 two-string
    // File/Env reference otherwise. A `new_source_json` line on a build without
    // serde fails closed (it cannot be deserialized) rather than silently
    // dropping a real in-flight rotation.
    let new_source = if let Some(json) = new_source_json {
        reconstruct_new_source_json(&json, &corrupt)?
    } else {
        let value = value.ok_or_else(|| corrupt("missing new_source_value"))?;
        match kind.as_deref() {
            Some("file") => KeyProviderConfig::File { path: value.into() },
            Some("env") => KeyProviderConfig::Env { variable: value },
            Some(other) => return Err(corrupt(&format!("unknown new_source_kind {other:?}"))),
            None => return Err(corrupt("missing new_source_kind")),
        }
    };
    Ok(Some(RotationLedger {
        direction,
        new_version,
        new_source,
        index: layer_index.unwrap_or(def_index),
        checkpoint: layer_checkpoint.unwrap_or(def_checkpoint),
        wal: layer_wal.unwrap_or(def_wal),
        cold: layer_cold.unwrap_or(def_cold),
        // Absent `layer.wal_retire` (any legacy ledger, v1 or a pre-#3616-PR3 v2)
        // defaults to `Skipped`: the plaintext-WAL-retire step did not exist, so a
        // resume must NOT delete any WAL segment based on an ambiguous absent field.
        wal_retire: layer_wal_retire.unwrap_or(LayerStatus::Skipped),
        mek_kcv,
        // Absent `layer.subject_keyring` (any pre-Slice-5 ledger) defaults to
        // `Skipped`, mirroring `wal_retire`: a pre-Slice-5 rotation never re-wrapped
        // the subject keyring, so a resume must never fabricate a Pending re-wrap
        // pass from an ambiguous absent field.
        subject_keyring: layer_subject_keyring.unwrap_or(LayerStatus::Skipped),
        // Issue #3708: absent `algorithm=` (any pre-#3708 ledger, or a
        // rotation/disable ledger) loads `None`; the enable-resume verifier fails
        // closed on `None`.
        algorithm,
    }))
}

/// Deserialize a v3 `new_source_json` value back into a [`KeyProviderConfig`]
/// (Issue #3620). Present-but-unparseable JSON fails closed as `corrupt`.
#[cfg(feature = "serde")]
fn reconstruct_new_source_json(
    json: &str,
    corrupt: &impl Fn(&str) -> Error,
) -> Result<KeyProviderConfig> {
    serde_json::from_str::<KeyProviderConfig>(json)
        .map_err(|e| corrupt(&format!("unparseable new_source_json ({e})")))
}

/// Non-serde builds cannot deserialize a v3 ledger; fail closed rather than
/// mis-resume. (A v3 ledger can only have been written by a serde build.)
#[cfg(not(feature = "serde"))]
fn reconstruct_new_source_json(
    _json: &str,
    corrupt: &impl Fn(&str) -> Error,
) -> Result<KeyProviderConfig> {
    Err(corrupt(
        "version=3 rotation.state requires the serde feature to reconstruct new_source",
    ))
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
/// 1. **Pending rotation ledger** — the provisioned version is resolved
///    per-direction, because `new_version` does NOT mean the same thing for
///    every direction:
///    * `Forward` / `Cancel` — the currently CONFIGURED key still corresponds
///      to the OLD generation (the provider switch happens only after a
///      rotation COMPLETES), and `new_version = old_version + 1` by
///      construction, so we provision to `ledger.new_version - 1` (the old
///      version) and let startup [`resume_pending_rotation`] drive the keyring
///      to the ledger target. Provisioning to the *max on-disk* here would
///      instead be the target itself and would mis-label the configured (old)
///      cipher as the new version, breaking resume; the ledger target is
///      therefore the authority for the *final* `current_version` (reached via
///      resume), not the build version. This holds for a `cancel` ledger too
///      (old = target - 1).
///    * `Disable` — a `disable_scope` ledger records `new_version =
///      current_version` (the key still protecting the on-disk ciphertext, the
///      key a resumed disable DECRYPTS with — see [`RotationDirection::Disable`]),
///      NOT `old + 1`. Subtracting 1 here would provision `current_version - 1`
///      and leave startup unable to decrypt files stamped `current_version`
///      after a mid-disable crash, so we provision to `ledger.new_version`
///      verbatim.
///    * `Enable` — a plaintext → encrypted ledger has `new_version == BASE`, so
///      `new_version` (no subtraction) equals the base and matches the old
///      formula's result; kept in the exhaustive match for clarity.
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
        let version = match ledger.direction {
            // Issue #3680: shared with the cancel driver so the two can never
            // disagree about which generation a pending rotation came from.
            RotationDirection::Forward | RotationDirection::Cancel => ledger.rotation_old_version(),
            // A disable ledger's `new_version` is the CURRENT (to-be-retired)
            // key version protecting on-disk ciphertext, not `old + 1`; an
            // enable ledger's is `BASE`. Both provision to `new_version` as-is.
            RotationDirection::Enable | RotationDirection::Disable => ledger.new_version,
        };
        return Ok(version);
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
    // A plaintext → encrypted ENABLE ledger (Issue #3616 PR3) is driven by the
    // enable engine's own pre-read hook + `resume_pending_enable`, never by the
    // rotation resume paths (which assume an already-encrypted old→new gen).
    if matches!(
        ledger.direction,
        RotationDirection::Enable | RotationDirection::Disable
    ) {
        return Ok(());
    }
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
    let new_wal_dek = derive_wal_dek(load_mek_checked(&ledger)?).map_err(rotation_err)?;
    let new_wal_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_wal_dek));
    // Re-assert the old generation (flips the keyring to strict per-version
    // dispatch) then add the new one, so legacy/old-kv segments resolve to the
    // old DEK and new-kv segments to the new DEK.
    wal_keyring.add_generation(old_version, Arc::clone(&old_cipher));
    wal_keyring.add_generation(ledger.new_version, new_wal_cipher);
    Ok(())
}

// ── Plaintext → encrypted ENABLE engine ledger helpers (Issue #3616 PR3) ──────
//
// The enable engine reuses the cross-layer rotation ledger + `LayerStatus`, but
// records `direction=enable` (see [`RotationDirection::Enable`]) so the rotation
// resume paths never touch it. These `pub(crate)` helpers are the seam the
// engine in `crate::db::encryption_enable` drives; all ledger internals stay
// private to this module.

/// The fresh-enable key generation. A plaintext database has no prior key
/// version, so its first (and only) generation is the base version, matching the
/// WAL's [`INITIAL_WAL_KEY_VERSION`](crate::encryption::wal_encryption::INITIAL_WAL_KEY_VERSION)
/// and the index base [`ENC_INDEX_KEY_VERSION_V1`] (both `1`).
pub(crate) const ENABLE_KEY_VERSION: u32 = ENC_INDEX_KEY_VERSION_V1;

/// Per-field view of an in-flight enable ledger for the resume driver — exposes
/// the recorded key source and each layer's pending/complete state WITHOUT
/// leaking the private [`RotationLedger`] / [`LayerStatus`] types.
pub(crate) struct EnableLedgerView {
    /// The File/Env key-source reference recorded in the ledger.
    pub(crate) new_source: KeyProviderConfig,
    /// `true` while the WAL layer still needs its keyring install + roll.
    pub(crate) wal_pending: bool,
    /// `true` once the WAL layer's roll+retire is durably complete.
    pub(crate) wal_complete: bool,
    /// `true` while the index layer still needs its plaintext→AEIX wrap pass.
    pub(crate) index_pending: bool,
    /// `true` once the index layer's plaintext→AEIX wrap is durably complete —
    /// i.e. the encrypted snapshot holds the pre-enable state, so it is safe to
    /// retire the pre-enable plaintext WAL segments.
    pub(crate) index_complete: bool,
    /// `true` while the checkpoint layer is still pending.
    pub(crate) checkpoint_pending: bool,
    /// `true` while the cold layer still needs its bare→ACV1 wrap pass.
    pub(crate) cold_pending: bool,
    /// `true` while the pre-enable plaintext WAL segments still need retiring
    /// (Issue #3616 PR3). Only ever set for an enable ledger whose index layer is
    /// in scope (there is a durable snapshot to hold the pre-enable state first).
    pub(crate) wal_retire_pending: bool,
    /// The KCV of the new MEK stamped when the enable was started (Issue #3620),
    /// or `None` on a legacy/non-serde ledger. A crash-resume verifies the MEK
    /// re-derived from `new_source` against this BEFORE any wrap pass, so a
    /// changed source secret is refused precisely instead of silently wrapping
    /// the still-plaintext files under a different key.
    pub(crate) mek_kcv: Option<String>,
    /// The CONCRETE (resolved) AEAD algorithm the enable pinned at start (Issue
    /// #3708), or `None` for a legacy enable ledger written before pinning existed.
    /// Every resume path verifies the now-resolved algorithm against this via
    /// [`verify_resumed_enable_algorithm`] BEFORE any wrap/unwrap, failing closed on
    /// a mismatch (or on `None`) so a cross-host/config resume can never wrap the
    /// remaining files under a different algorithm than the already-wrapped ones.
    pub(crate) algorithm: Option<Algorithm>,
}

impl EnableLedgerView {
    fn from_ledger(ledger: RotationLedger) -> Self {
        Self {
            wal_pending: matches!(ledger.wal, LayerStatus::Pending),
            wal_complete: matches!(ledger.wal, LayerStatus::Complete),
            index_pending: matches!(ledger.index, LayerStatus::Pending),
            index_complete: matches!(ledger.index, LayerStatus::Complete),
            checkpoint_pending: matches!(ledger.checkpoint, LayerStatus::Pending),
            cold_pending: matches!(ledger.cold, LayerStatus::Pending),
            wal_retire_pending: matches!(ledger.wal_retire, LayerStatus::Pending),
            new_source: ledger.new_source,
            mek_kcv: ledger.mek_kcv,
            algorithm: ledger.algorithm,
        }
    }

    /// Every non-WAL layer is settled (none still `Pending`), so the only
    /// remaining enable work is the authority flip + ledger clear.
    ///
    /// The enable engine completes the index + checkpoint layers *inline* (their
    /// plaintext → `AEIX` wrap pass runs in `resume_pending_enable`, before the
    /// index directory is read back), so once that pass marks them `Complete`
    /// this predicate depends only on whether the cold layer is still `Pending`.
    /// Cold is completed in a *later* phase (`resume_pending_enable_cold`, after
    /// the cold store is wired), so a cold-in-scope resume keeps the ledger and
    /// defers the flip until that phase.
    ///
    /// The plaintext-WAL-retire cleanup (`wal_retire`) is also included: the
    /// authority must not flip until the pre-enable plaintext WAL is retired (the
    /// retire runs inline in `resume_pending_enable`, right after the index wrap).
    pub(crate) fn non_wal_layers_settled(&self) -> bool {
        !self.index_pending
            && !self.checkpoint_pending
            && !self.cold_pending
            && !self.wal_retire_pending
    }
}

/// Read the in-flight ENABLE ledger for `manager`, if one is present. Returns
/// `Ok(None)` when absent OR when a ledger is present but is a rotation (not an
/// enable) ledger. Fail-closed on a corrupt ledger (propagates the error).
pub(crate) fn read_enable_ledger(
    manager: &IndexPersistenceManager,
) -> Result<Option<EnableLedgerView>> {
    Ok(read_rotation_state(manager)?
        .filter(|l| matches!(l.direction, RotationDirection::Enable))
        .map(EnableLedgerView::from_ledger))
}

/// Path-based twin of [`read_enable_ledger`] for the pre-manager startup hook.
pub(crate) fn read_enable_ledger_at(path: &std::path::Path) -> Result<Option<EnableLedgerView>> {
    Ok(read_rotation_state_at(path)?
        .filter(|l| matches!(l.direction, RotationDirection::Enable))
        .map(EnableLedgerView::from_ledger))
}

/// Durably write the pending-enable ledger BEFORE any layer is touched (Issue
/// #3616 PR3). WAL is always `Pending`; index/checkpoint/cold are `Pending` iff
/// their `*_in_scope` bytes exist, else `Skipped`. Same crash-safe ordering as
/// the rotation ledger (temp → fsync → rename → parent-dir fsync).
pub(crate) fn write_enable_ledger(
    manager: &IndexPersistenceManager,
    new_source: &KeyProviderConfig,
    index_in_scope: bool,
    cold_in_scope: bool,
    algorithm: Algorithm,
) -> Result<()> {
    let mut ledger = RotationLedger::enable_scope(
        ENABLE_KEY_VERSION,
        new_source.clone(),
        index_in_scope,
        cold_in_scope,
        algorithm,
    );
    // Stamp the KCV of the new MEK so a crash-resume can detect a wrong/changed
    // key at the source precisely (Issue #3620).
    attach_kcv_from_source(&mut ledger, new_source)?;
    write_ledger(manager, &ledger)
}

/// Build the WAL DEK keyring to install for an enable, derived from the recorded
/// key `source` (`MEK → HKDF wal DEK → cipher`), stamped at
/// [`ENABLE_KEY_VERSION`]. No key material leaves `Zeroizing`.
pub(crate) fn build_enable_wal_keyring(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<WalKeyring> {
    let wal_dek = derive_wal_dek(load_mek(source).map_err(rotation_err)?).map_err(rotation_err)?;
    let cipher: Arc<dyn Cipher> = Arc::from(create_cipher(algorithm, &wal_dek));
    Ok(WalKeyring::single_versioned(cipher, ENABLE_KEY_VERSION))
}

/// Install the WAL keyring for an INTERRUPTED enable BEFORE the startup replay
/// read (Issue #3616 PR3) — the crux crash-consistency hook.
///
/// A crash after the enable rolled the WAL to encrypted (v16) but before the
/// authority flip leaves `config.encryption.enabled == false`, so the WAL was
/// built with a `None` keyring and the replay read would hit encrypted frames
/// with no cipher and fail — mis-reading encrypted bytes as (undecodable)
/// plaintext. This hook, run in the `!config.encryption.enabled` startup branch,
/// detects a pending ENABLE ledger whose WAL layer is `Pending`/`Complete` and
/// installs the keyring (derived from the ledger's recorded source) via the PR2
/// `install_wal_keyring` seam, so the read decrypts. Idempotent / fail-closed: a
/// no-op when no enable ledger is pending or the WAL is already encrypted.
pub(crate) fn install_pending_enable_wal_keyring(
    wal: &ConcurrentWalSystem,
    enc_cfg: &EncryptionConfig,
    data_dir: &std::path::Path,
) -> Result<()> {
    let ledger_path = data_dir.join("rotation.state");
    let Some(view) = read_enable_ledger_at(&ledger_path)? else {
        return Ok(());
    };
    // The WAL is in the encrypted picture whenever its layer is Pending (roll may
    // have started) or Complete (roll finished). Already-encrypted → nothing to do.
    if !(view.wal_pending || view.wal_complete) || wal.is_encrypted() {
        return Ok(());
    }
    // Issue #3708 (MEDIUM): verify the pinned algorithm AND the source-secret KCV
    // BEFORE building/installing the keyring, because installing SEALS the active
    // plaintext segment and FORCE-ROLLS a fresh encrypted segment — an irreversible
    // side effect. A changed algorithm or out-of-band-changed source secret must
    // refuse HERE (ledger retained, no seal/roll) so the resume surfaces the precise
    // KCV/algorithm error and leaves NO stray wrong-key segment on disk — instead of
    // the pre-fix behavior that sealed a wrong-key segment and only failed later at
    // the KCV check in `resume_pending_enable`, bricking the DB even with the correct
    // key restored.
    let algorithm = verify_resumed_enable_algorithm(view.algorithm, enc_cfg.algorithm)?;
    verify_resumed_source_kcv(&view.new_source, view.mek_kcv.as_deref())?;
    let keyring = build_enable_wal_keyring(&view.new_source, algorithm)?;
    wal.install_wal_keyring(keyring)
}

/// Build the index DEK cipher for an enable, derived from the recorded key
/// `source` (`MEK → HKDF index DEK → cipher`). Checkpoints ride this same DEK and
/// format. No key material leaves `Zeroizing`.
pub(crate) fn build_enable_index_cipher(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<Arc<dyn Cipher>> {
    let index_dek =
        derive_index_dek(load_mek(source).map_err(rotation_err)?).map_err(rotation_err)?;
    Ok(Arc::from(create_cipher(algorithm, &index_dek)))
}

/// Build the cold-storage DEK cipher for an enable, derived from the recorded key
/// `source` (`MEK → HKDF cold DEK → cipher`). No key material leaves `Zeroizing`.
pub(crate) fn build_enable_cold_cipher(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<Arc<dyn Cipher>> {
    let cold_dek =
        derive_cold_dek(load_mek(source).map_err(rotation_err)?).map_err(rotation_err)?;
    Ok(Arc::from(create_cipher(algorithm, &cold_dek)))
}

/// Run the plaintext → `AEIX` wrap pass over the manager's `indexes/` directory
/// for an enable (Issue #3616 PR3), stamping [`ENABLE_KEY_VERSION`]. Idempotent /
/// resumable: files already carrying the `AEIX` header are skipped. Checkpoint
/// files ride the index DEK/format and are covered by this same pass.
pub(crate) fn wrap_enable_index_files(
    manager: &IndexPersistenceManager,
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<()> {
    let cipher = build_enable_index_cipher(source, algorithm)?;
    crate::storage::index_persistence::reencrypt::wrap_plaintext_index_dir(
        &manager.indexes_path(),
        &cipher,
        ENABLE_KEY_VERSION,
    )
    .map_err(rotation_err)?;
    Ok(())
}

/// Rewrite the durable ledger flipping BOTH `layer.index` and `layer.checkpoint`
/// to `complete` (Issue #3616 PR3), preserving every other field. Checkpoints
/// ride the index DEK/format, so the index wrap pass settles both at once. A
/// no-op when no ledger is present.
pub(crate) fn mark_index_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) = read_rotation_state(manager)? {
        let mut changed = false;
        if ledger.index != LayerStatus::Complete {
            ledger.index = LayerStatus::Complete;
            changed = true;
        }
        if ledger.checkpoint != LayerStatus::Complete {
            ledger.checkpoint = LayerStatus::Complete;
            changed = true;
        }
        if changed {
            write_ledger(manager, &ledger)?;
        }
    }
    Ok(())
}

/// Rewrite the durable ledger flipping `layer.wal_retire=complete` (Issue #3616
/// PR3), preserving every other field. Recorded AFTER the pre-enable plaintext WAL
/// segments are retired, so a crash mid-retire leaves it `Pending` and the resume
/// re-runs the (idempotent) retire. A no-op when no ledger is present.
pub(crate) fn mark_wal_retire_complete(manager: &IndexPersistenceManager) -> Result<()> {
    if let Some(mut ledger) = read_rotation_state(manager)?
        && ledger.wal_retire != LayerStatus::Complete
    {
        ledger.wal_retire = LayerStatus::Complete;
        write_ledger(manager, &ledger)?;
    }
    Ok(())
}

/// Retire the pre-enable **plaintext** (v13) WAL segments after an enable has
/// rolled the WAL to encrypted AND durably captured the pre-enable state into the
/// encrypted (`AEIX`) index snapshot (Issue #3616 PR3).
///
/// # Safety (no data loss)
///
/// The caller MUST guarantee the encrypted index snapshot already holds every
/// pre-enable record (as of the seal LSN) before calling this — the enable engine
/// does a synchronous `persist_indexes()` while the DB is still plaintext and then
/// wraps that snapshot to `AEIX`, and the resume path relies on that same durable
/// snapshot plus the startup replay having already read every segment into memory.
/// [`retire_old_generation_segments`] keeps the active (fresh encrypted) segment
/// and removes only the sealed legacy (`(_, None)`) plaintext segments, so no
/// cleartext survives at rest while the reopen still reconstructs from the
/// snapshot. Idempotent: a re-run after a crash mid-retire removes any remaining
/// old-generation segments.
///
/// [`retire_old_generation_segments`]: crate::storage::wal::concurrent_system::ConcurrentWalSystem::retire_old_generation_segments
pub(crate) fn retire_enable_plaintext_wal(wal: &ConcurrentWalSystem) -> Result<usize> {
    wal.retire_old_generation_segments(ENABLE_KEY_VERSION)
}

/// The at-rest ciphers an INTERRUPTED plaintext → encrypted enable must build its
/// layers under on the resuming `open()` (Issue #3616 PR3).
///
/// A crash before the authority flip leaves `config.encryption.enabled == false`,
/// so `open()` would build the index manager and cold store *plaintext* — and
/// then fail to read (or, worse, over-write with plaintext) the bytes the resume
/// wrap passes are turning into `AEIX` / `ACV1`. This bundle lets `open()` build
/// those layers under the enable DEKs derived from the pending ledger's recorded
/// source, so the resumed session reads its own freshly-wrapped bytes. The WAL is
/// handled separately by [`install_pending_enable_wal_keyring`].
pub(crate) struct EnableResumeCiphers {
    /// The index DEK cipher (checkpoints ride it).
    pub(crate) index: Arc<dyn Cipher>,
    /// The cold-storage DEK cipher.
    pub(crate) cold: Arc<dyn Cipher>,
}

/// Resolve the [`EnableResumeCiphers`] for a pending ENABLE ledger at `data_dir`,
/// or `Ok(None)` when no enable migration is pending (the common path). Called on
/// the `!config.encryption.enabled` startup branch (authority not yet flipped) so
/// the index manager + cold store are built under the enable DEKs. Fail-closed on
/// a corrupt ledger.
pub(crate) fn enable_resume_ciphers(
    data_dir: &std::path::Path,
    algorithm: Algorithm,
) -> Result<Option<EnableResumeCiphers>> {
    let ledger_path = data_dir.join("rotation.state");
    let Some(view) = read_enable_ledger_at(&ledger_path)? else {
        return Ok(None);
    };
    // Issue #3708: this is the EARLIEST resume touch-point (it builds the ciphers
    // `open()` reads the at-rest layers under). Verify the pinned algorithm matches
    // the resume host/config and build under the ledger's CONCRETE pinned algorithm,
    // so a cross-host/config divergence refuses here (ledger retained, startup
    // aborts) before any wrap can split the tier.
    let resolved = verify_resumed_enable_algorithm(view.algorithm, algorithm)?;
    Ok(Some(EnableResumeCiphers {
        index: build_enable_index_cipher(&view.new_source, resolved)?,
        cold: build_enable_cold_cipher(&view.new_source, resolved)?,
    }))
}

// ── Encrypted → plaintext DISABLE engine ledger helpers (Issue #3616 PR4) ──────
//
// The exact mirror of the ENABLE helpers above. The disable engine reuses the
// cross-layer rotation ledger + `LayerStatus`, but records `direction=disable`
// (see [`RotationDirection::Disable`]) so the rotation resume paths never touch
// it. On a disable ledger `new_version` / `new_source` name the CURRENT
// (to-be-retired) key coordinates — the key a resumed disable DECRYPTS each layer
// with. These `pub(crate)` helpers are the seam the engine in
// `crate::db::encryption_disable` drives; all ledger internals stay private here.

/// The keep-version sentinel a disable WAL retire passes to
/// [`retire_old_generation_segments`](ConcurrentWalSystem::retire_old_generation_segments):
/// `0` is never a real key generation (both index and WAL bases are `1`), so every
/// keyversioned (encrypted) sealed segment classifies as "old generation" and is
/// deleted, while the fresh PLAINTEXT active segment is kept by the retire's own
/// current-segment protection. This is how disable retires the encrypted WAL tail
/// while keeping the post-uninstall plaintext segment.
const DISABLE_WAL_RETIRE_KEEP_VERSION: u32 = 0;

/// Per-field view of an in-flight DISABLE ledger for the resume driver — the
/// mirror of [`EnableLedgerView`], exposing the recorded (to-be-retired) key
/// source + version and each layer's pending/complete state WITHOUT leaking the
/// private [`RotationLedger`] / [`LayerStatus`] types.
pub(crate) struct DisableLedgerView {
    /// The File/Env key-source reference the on-disk ciphertext is protected by
    /// (the key a resumed disable DECRYPTS with).
    pub(crate) current_source: KeyProviderConfig,
    /// The key version the on-disk encrypted bytes are stamped with (used to
    /// build the decrypt WAL / cold keyrings at the correct generation).
    pub(crate) current_version: u32,
    /// `true` while the index layer still needs its `AEIX` → plaintext unwrap.
    pub(crate) index_pending: bool,
    /// `true` while the checkpoint layer is still pending.
    pub(crate) checkpoint_pending: bool,
    /// `true` while the cold layer still needs its `ACV1` → bare unwrap pass.
    pub(crate) cold_pending: bool,
    /// `true` while the pre-disable ENCRYPTED WAL segments still need retiring.
    pub(crate) wal_retire_pending: bool,
    /// `true` once the encrypted WAL segments have been retired — the signal that
    /// the WAL is fully plaintext at rest and needs no decrypt keyring to replay.
    pub(crate) wal_retire_complete: bool,
    /// The KCV of the CURRENT (to-be-retired) MEK stamped when the disable was
    /// started (Issue #3620), or `None` on a legacy/non-serde ledger. A
    /// crash-resume verifies the MEK re-derived from `current_source` against
    /// this BEFORE any unwrap pass, turning a changed source secret into a
    /// precise error rather than a cryptic downstream AEAD decrypt failure.
    pub(crate) mek_kcv: Option<String>,
}

impl DisableLedgerView {
    fn from_ledger(ledger: RotationLedger) -> Self {
        Self {
            current_version: ledger.new_version,
            index_pending: matches!(ledger.index, LayerStatus::Pending),
            checkpoint_pending: matches!(ledger.checkpoint, LayerStatus::Pending),
            cold_pending: matches!(ledger.cold, LayerStatus::Pending),
            wal_retire_pending: matches!(ledger.wal_retire, LayerStatus::Pending),
            wal_retire_complete: matches!(ledger.wal_retire, LayerStatus::Complete),
            current_source: ledger.new_source,
            mek_kcv: ledger.mek_kcv,
        }
    }

    /// Every non-WAL layer is settled (none still `Pending`), so the only
    /// remaining disable work is the authority flip + ledger clear. The mirror of
    /// [`EnableLedgerView::non_wal_layers_settled`]: the index + checkpoint unwrap
    /// runs inline in `resume_pending_disable` (before the index dir is read back),
    /// while cold is completed in a later phase (`resume_pending_disable_cold`,
    /// after the cold store is wired), so a cold-in-scope resume keeps the ledger
    /// and defers the flip. `wal_retire` is included: the authority must not flip
    /// until the encrypted WAL tail is retired.
    pub(crate) fn non_wal_layers_settled(&self) -> bool {
        !self.index_pending
            && !self.checkpoint_pending
            && !self.cold_pending
            && !self.wal_retire_pending
    }

    /// Whether the on-disk WAL may still carry ENCRYPTED segments needing a
    /// decrypt keyring for the startup replay read — true until the encrypted
    /// tail has been retired. Once `wal_retire` is `Complete` the WAL is plaintext
    /// at rest and installing a keyring would needlessly RE-ENCRYPT the fresh
    /// active segment.
    pub(crate) fn wal_encrypted_on_disk(&self) -> bool {
        !self.wal_retire_complete
    }
}

/// Read the in-flight DISABLE ledger for `manager`, if one is present. Returns
/// `Ok(None)` when absent OR when a present ledger is a rotation / enable (not a
/// disable) ledger. Fail-closed on a corrupt ledger (propagates the error).
pub(crate) fn read_disable_ledger(
    manager: &IndexPersistenceManager,
) -> Result<Option<DisableLedgerView>> {
    Ok(read_rotation_state(manager)?
        .filter(|l| matches!(l.direction, RotationDirection::Disable))
        .map(DisableLedgerView::from_ledger))
}

/// Path-based twin of [`read_disable_ledger`] for the pre-manager startup hook.
pub(crate) fn read_disable_ledger_at(path: &std::path::Path) -> Result<Option<DisableLedgerView>> {
    Ok(read_rotation_state_at(path)?
        .filter(|l| matches!(l.direction, RotationDirection::Disable))
        .map(DisableLedgerView::from_ledger))
}

/// Durably write the pending-disable ledger BEFORE any layer is touched (Issue
/// #3616 PR4). WAL is always `Pending`; index/checkpoint/cold are `Pending` iff
/// their `*_in_scope` bytes exist, else `Skipped`. `current_version` /
/// `current_source` record the CURRENT (to-be-retired) key coordinates so a
/// resumed disable can rebuild the decrypt cipher. Same crash-safe ordering as
/// the enable ledger (temp → fsync → rename → parent-dir fsync).
pub(crate) fn write_disable_ledger(
    manager: &IndexPersistenceManager,
    current_source: &KeyProviderConfig,
    current_version: u32,
    index_in_scope: bool,
    cold_in_scope: bool,
) -> Result<()> {
    let mut ledger = RotationLedger::disable_scope(
        current_version,
        current_source.clone(),
        index_in_scope,
        cold_in_scope,
    );
    // Stamp the KCV of the CURRENT (to-be-retired) MEK so a resumed disable that
    // re-derives the decrypt key from the recorded source can detect a changed
    // key precisely (Issue #3620).
    attach_kcv_from_source(&mut ledger, current_source)?;
    write_ledger(manager, &ledger)
}

/// Build the WAL DEK keyring to install for a DISABLE resume — the DECRYPT keyring
/// derived from the recorded (to-be-retired) key `source`, stamped at the recorded
/// `version` so `read_from` decrypts each still-encrypted segment. No key material
/// leaves `Zeroizing`. (The disable DRIVER does not need this — it strips the WAL
/// via [`ConcurrentWalSystem::uninstall_wal_keyring`] on the live encrypted WAL —
/// only the resume path, whose WAL was built plaintext, does.)
pub(crate) fn build_disable_wal_keyring(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
    version: u32,
) -> Result<WalKeyring> {
    let wal_dek = derive_wal_dek(load_mek(source).map_err(rotation_err)?).map_err(rotation_err)?;
    let cipher: Arc<dyn Cipher> = Arc::from(create_cipher(algorithm, &wal_dek));
    Ok(WalKeyring::single_versioned(cipher, version))
}

/// Build the index DEK cipher for a disable, derived from the recorded key
/// `source` — the cipher the `AEIX` → plaintext unwrap pass decrypts with (a
/// single index DEK decrypts every version the enable wrap stamped; the version is
/// authenticated as AAD, not used to select the key). Checkpoints ride this same
/// DEK/format. Delegates to the shared derivation. No key material leaves
/// `Zeroizing`.
pub(crate) fn build_disable_index_cipher(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<Arc<dyn Cipher>> {
    build_enable_index_cipher(source, algorithm)
}

/// Build the cold-storage DEK cipher for a disable, derived from the recorded key
/// `source` — the cipher the cold store is built under so the `ACV1` → bare unwrap
/// pass can decrypt each wrapped value. Delegates to the shared derivation. No key
/// material leaves `Zeroizing`.
pub(crate) fn build_disable_cold_cipher(
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<Arc<dyn Cipher>> {
    build_enable_cold_cipher(source, algorithm)
}

/// Install the DECRYPT WAL keyring for an INTERRUPTED disable BEFORE the startup
/// replay read (Issue #3616 PR4) — the mirror of
/// [`install_pending_enable_wal_keyring`].
///
/// A crash mid-disable leaves the authority STILL enabled, but `open()` builds the
/// at-rest layers PLAINTEXT (the disable end state — see `db::config`), so the WAL
/// is built with NO keyring and the replay read would hit the still-encrypted
/// segments with no cipher and fail. This hook detects a pending DISABLE ledger
/// whose encrypted WAL tail has not yet been retired and installs the keyring
/// (derived from the ledger's recorded source + version) via the PR2
/// `install_wal_keyring` seam, so the read decrypts. `resume_pending_disable` then
/// uninstalls it back to plaintext + retires the encrypted tail. Idempotent /
/// fail-closed: a no-op when no disable ledger is pending, the encrypted tail is
/// already retired (installing would needlessly re-encrypt), or the WAL is already
/// encrypted (double-install guard).
pub(crate) fn install_pending_disable_wal_keyring(
    wal: &ConcurrentWalSystem,
    enc_cfg: &EncryptionConfig,
    data_dir: &std::path::Path,
) -> Result<()> {
    let ledger_path = data_dir.join("rotation.state");
    let Some(view) = read_disable_ledger_at(&ledger_path)? else {
        return Ok(());
    };
    if !view.wal_encrypted_on_disk() || wal.is_encrypted() {
        return Ok(());
    }
    let keyring = build_disable_wal_keyring(
        &view.current_source,
        enc_cfg.algorithm,
        view.current_version,
    )?;
    wal.install_wal_keyring(keyring)
}

/// Retire the pre-disable ENCRYPTED WAL segments after the keyring has been
/// uninstalled (Issue #3616 PR4) — the mirror of [`retire_enable_plaintext_wal`].
///
/// After [`uninstall_wal_keyring`](ConcurrentWalSystem::uninstall_wal_keyring) the
/// WAL directory holds sealed ENCRYPTED (v16) segments plus one fresh PLAINTEXT
/// (v13) active segment. This retires the encrypted tail (now undecryptable
/// through the plaintext live cell, and no longer needed because the durable index
/// snapshot holds every pre-disable record) while keeping the plaintext active
/// segment (the retire protects the current segment by id). Idempotent: a re-run
/// after a crash mid-retire removes any remaining encrypted segments.
///
/// # Safety (no data loss)
///
/// The caller MUST guarantee the durable index snapshot already holds every
/// pre-disable record before calling this — the disable engine does a synchronous
/// `persist_indexes()` while the WAL is still encrypted (capturing the `AEIX`
/// snapshot) BEFORE writing the disable ledger, and the resume path additionally
/// has the startup replay read every segment into memory first.
pub(crate) fn retire_disable_encrypted_wal(wal: &ConcurrentWalSystem) -> Result<usize> {
    wal.retire_old_generation_segments(DISABLE_WAL_RETIRE_KEEP_VERSION)
}

/// Run the `AEIX` → plaintext unwrap pass over the manager's `indexes/` directory
/// for a disable (Issue #3616 PR4), decrypting under the index DEK derived from
/// `source`. Idempotent / resumable: files already bare are skipped. Checkpoint
/// files ride the index DEK/format and are covered by this same pass. The mirror
/// of [`wrap_enable_index_files`].
pub(crate) fn unwrap_disable_index_files(
    manager: &IndexPersistenceManager,
    source: &KeyProviderConfig,
    algorithm: Algorithm,
) -> Result<()> {
    let cipher = build_disable_index_cipher(source, algorithm)?;
    crate::storage::index_persistence::reencrypt::unwrap_encrypted_index_dir(
        &manager.indexes_path(),
        &cipher,
    )
    .map_err(rotation_err)?;
    Ok(())
}

/// The at-rest DECRYPT ciphers an INTERRUPTED encrypted → plaintext disable must
/// build its still-encrypted layers under on the resuming `open()` (Issue #3616
/// PR4) — the mirror of [`EnableResumeCiphers`], inverted.
///
/// A crash mid-disable leaves the authority STILL enabled, so `open()` would
/// normally build every layer ENCRYPTED — but the disable end state is PLAINTEXT,
/// and a live encrypted index manager / cold store would RE-ENCRYPT over the
/// freshly-unwrapped bytes on the next background persist / cold migration. So
/// `db::config` forces the layers PLAINTEXT (the end state) when this resolves to
/// `Some`, and uses these ciphers only for the transient read passes: the cold
/// store is built under `cold` (at `version`) so its `ACV1` → bare unwrap can
/// decrypt. (The index unwrap re-derives its cipher from the ledger's recorded
/// source in [`unwrap_disable_index_files`], reading the `AEIX` files directly, so
/// it needs no cipher threaded here; and the WAL is handled separately by
/// [`install_pending_disable_wal_keyring`].)
pub(crate) struct DisableResumeCiphers {
    /// The cold-storage DEK cipher the cold store is built under to unwrap `ACV1`.
    pub(crate) cold: Arc<dyn Cipher>,
    /// The key version the on-disk ciphertext is stamped with (the cold keyring is
    /// built at this generation).
    pub(crate) version: u32,
}

/// Resolve the [`DisableResumeCiphers`] for a pending DISABLE ledger at `data_dir`,
/// or `Ok(None)` when no disable migration is pending (the common path). Called on
/// the encrypted-authority startup branch (authority not yet flipped to disabled)
/// so `db::config` can force the layers plaintext and thread the decrypt cold
/// cipher into the transient resume read pass. Fail-closed on a corrupt ledger.
pub(crate) fn disable_resume_ciphers(
    data_dir: &std::path::Path,
    algorithm: Algorithm,
) -> Result<Option<DisableResumeCiphers>> {
    let ledger_path = data_dir.join("rotation.state");
    let Some(view) = read_disable_ledger_at(&ledger_path)? else {
        return Ok(None);
    };
    Ok(Some(DisableResumeCiphers {
        cold: build_disable_cold_cipher(&view.current_source, algorithm)?,
        version: view.current_version,
    }))
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
///   Refused up front (Issue #3783, ledger retained) when the cancel ledger
///   records `layer.wal` / `layer.cold` / `layer.subject_keyring` as anything
///   other than `Skipped`: the reverse pass covers only the index/checkpoint
///   tree, so completing it would leave those layers on the new key while
///   reporting success.
///
/// A no-op when no rotation was in flight. A present-but-corrupt breadcrumb
/// aborts startup (Issue #488 P1.1).
///
/// # Errors
///
/// Returns an error if the breadcrumb is corrupt, the recorded new key source
/// cannot be sourced, a cancel ledger records a layer the reverse pass cannot roll
/// back ([`RotationError::UnsupportedCancelWithRekeyedLayers`]), or the
/// re-encryption pass fails.
pub fn resume_pending_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
    wal: Option<&ConcurrentWalSystem>,
    wal_persist: Option<&dyn Fn() -> Result<()>>,
) -> Result<Option<RotationReport>> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(None);
    };
    // An ENABLE ledger (Issue #3616 PR3) is not a rotation — leave it for
    // `resume_pending_enable`, never resume it as an old→new-generation rotation.
    if matches!(
        ledger.direction,
        RotationDirection::Enable | RotationDirection::Disable
    ) {
        return Ok(None);
    }
    // Issue #3783: the same refusal the live `cancel_pending_rotation` driver
    // applies, re-asserted here so resuming an interrupted cancel can never clear
    // the ledger and report success while a non-index layer sits on the new key.
    // Checked FIRST — before any keyring generation is installed and before the WAL
    // block below (which would otherwise roll the WAL *forward* off a cancel
    // ledger) — so the refusal leaves the ledger and every layer untouched. A
    // cancel marker written by this binary always records those layers `Skipped`
    // (the driver's guard runs before the marker is published), so this is a
    // fail-closed backstop for a hand-edited or future-written ledger.
    if matches!(ledger.direction, RotationDirection::Cancel) {
        let blocked = ledger.unrollbackable_layers();
        if !blocked.is_empty() {
            return Err(rotation_err(
                RotationError::UnsupportedCancelWithRekeyedLayers {
                    layers: blocked.join(", "),
                },
            ));
        }
    }
    let Some(keyring) = manager.keyring() else {
        // Encryption not configured on this startup; leave the ledger.
        return Ok(None);
    };
    let started = Instant::now();
    let old_cipher = keyring
        .current_cipher()
        .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
    let old_version = keyring.current_version();

    // Index DEK (index + checkpoint ride it). PR3 will add the cold DEK here.
    let new_dek = derive_index_dek(load_mek_checked(&ledger)?).map_err(rotation_err)?;
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
    // Subject-keyring layer (Slice 5): the LAST layer to settle. When it is still
    // `Pending`, this resume pass must NOT clear the ledger — the deferred
    // `finalize_resumed_subject_keyring_rotation` (run after the cold finalize)
    // performs the re-wrap and the final clear. Mirrors `cold_pending`.
    let subject_keyring_pending = matches!(ledger.subject_keyring, LayerStatus::Pending);
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
        let new_wal_dek = derive_wal_dek(load_mek_checked(&ledger)?).map_err(rotation_err)?;
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
            if !wal_finalize_deferred && !cold_pending && !subject_keyring_pending {
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
        // A `direction=enable` (or `direction=disable`, Issue #3616 PR4) ledger is
        // short-circuited to `Ok(None)` at the top of this function (the rotation
        // resume paths never drive a migration engine), so it can never reach this
        // match.
        RotationDirection::Enable | RotationDirection::Disable => {
            unreachable!("enable/disable ledgers are skipped before rotation resume")
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
    if matches!(
        ledger.direction,
        RotationDirection::Enable | RotationDirection::Disable
    ) {
        return Ok(());
    }
    if !matches!(ledger.wal, LayerStatus::Pending) {
        return Ok(());
    }
    let Some(wal_keyring) = wal.wal_keyring() else {
        return Ok(());
    };

    // Re-derive + re-install the new WAL generation (idempotent: it was already
    // installed by `resume_pending_rotation` / `install_pending_wal_generations`).
    let new_wal_dek = derive_wal_dek(load_mek_checked(&ledger)?).map_err(rotation_err)?;
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
    // cold values are still half-rotated, wedging a provider switch. Slice 5: the
    // subject-keyring layer settles even later, so also defer while it is
    // `Pending` (its finalize runs after the cold finalize and clears last).
    if !matches!(ledger.cold, LayerStatus::Pending)
        && !matches!(ledger.subject_keyring, LayerStatus::Pending)
    {
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
/// # Crash caveat: resume under the OLD key, THEN switch (inherited from PR2)
///
/// This is the inherent "resume under old key, then switch" contract of a
/// full-MEK rotation, structurally identical to PR2's WAL resume contract
/// (`install_pending_wal_generations`). While a rotation is `Pending`, the
/// **old** MEK must remain available to finish it: the cold store is opened at
/// its provisioned OLD generation, and the still-half-rotated `ACV1@base`
/// (old-DEK) values are decrypted with that old cold DEK during the resumed bulk
/// pass. If instead the operator reopens under the **new** key alone while a
/// rotation crashed mid-cold-pass, the surviving old-DEK values can no longer be
/// decrypted — the read is a **loud** AEAD authentication failure, so `open()`
/// returns `Err` and the DB is unopenable rather than silently returning wrong
/// data. Recovery is straightforward and lossless: reopen once under the OLD key
/// so this resume completes (re-wrapping every value at the new generation and
/// clearing the ledger), after which the new key alone reads everything. The
/// fault is therefore availability-only and fully recoverable, never integrity
/// loss.
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
    if matches!(
        ledger.direction,
        RotationDirection::Enable | RotationDirection::Disable
    ) {
        return Ok(());
    }
    if !matches!(ledger.cold, LayerStatus::Pending) {
        return Ok(());
    }
    if !cold.is_encrypted() {
        return Ok(());
    }

    // Re-derive + install the new cold generation (idempotent: a re-run replaces
    // the same-version generation). The old generation is the cold store's
    // provisioned single generation, still live for reading old-DEK values.
    let new_cold_dek = derive_cold_dek(load_mek_checked(&ledger)?).map_err(rotation_err)?;
    let new_cold_cipher: Arc<dyn Cipher> =
        Arc::from(create_cipher(enc_cfg.algorithm, &new_cold_dek));
    cold.install_cold_generation(ledger.new_version, new_cold_cipher)?;

    // Resume the bulk re-encrypt from the redb cursor (skips already-wrapped
    // values), retire the old generation, then complete + clear the ledger —
    // UNLESS the subject-keyring layer is still `Pending` (Slice 5), in which case
    // leave the ledger for `finalize_resumed_subject_keyring_rotation` to settle
    // last (mirrors how `finalize_resumed_wal_rotation` defers to cold).
    cold.reencrypt_cold_values(ledger.new_version)?;
    cold.retire_cold_generations(ledger.new_version)?;
    mark_cold_complete(manager)?;
    if !matches!(ledger.subject_keyring, LayerStatus::Pending) {
        clear_rotation_state(manager);
    }
    Ok(())
}

/// Finalize a startup- or live-resumed crypto-shred SUBJECT-KEYRING re-wrap
/// (Slice 5), completing the re-wrap pass and clearing the rotation ledger.
///
/// The subject keyring is the LAST layer to settle in a full-MEK rotation: it
/// runs after index / WAL / cold have all been re-keyed (each of those finalize
/// passes now defers the ledger clear while `layer.subject_keyring` is
/// `Pending`), so this function performs the final ledger clear. It is
/// idempotent and resumable:
///
/// 1. re-derive the OLD subject-wrap cipher from the CURRENT (live) MEK — during
///    a resume-under-old-key the encryption config still points at the old key —
///    and the NEW subject-wrap cipher from the ledger's recorded new source;
/// 2. run [`CryptoShredState::rewrap_keyring`], which skips erased subjects and
///    entries already at the target version, so a crash mid-pass resumes with no
///    double-wrap and no error;
/// 3. mark `layer.subject_keyring=complete` and clear the ledger.
///
/// A no-op (`Ok(())`) when no ledger is present, the layer is not `Pending`, or
/// the direction is `enable` — so `db::config` and the live resume can call it
/// unconditionally.
///
/// # Crash caveat: keep the OLD MEK available until this completes
///
/// Identical in spirit to the cold layer's contract
/// ([`finalize_resumed_cold_rotation`]): while `layer.subject_keyring` is
/// `Pending`, the OLD MEK must remain the live encryption source so the old
/// subject-wrap cipher can unwrap the still-old-generation per-subject DEKs
/// during this resumed pass. If the operator instead reopens under the NEW key
/// alone while the re-wrap crashed mid-pass, the surviving old-generation wrapped
/// DEKs can no longer be unwrapped — a **loud** AEAD authentication failure, so
/// the pass returns `Err` rather than silently corrupting the keyring. Recovery
/// is lossless: reopen once under the OLD key so this resume completes (re-wrapping
/// every entry at the new generation and clearing the ledger), after which the new
/// key alone unwraps everything. The fault is availability-only, never data loss.
///
/// # Errors
///
/// Returns an error if the old or new MEK cannot be sourced, or the re-wrap pass
/// fails (a genuine wrong/absent key surfaces loudly; the ledger is left
/// `Pending` for a later resume).
#[cfg(feature = "audit-export")]
pub fn finalize_resumed_subject_keyring_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
    crypto_shred: &crate::db::crypto_shred::CryptoShredState,
) -> Result<()> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(());
    };
    if matches!(ledger.direction, RotationDirection::Enable) {
        return Ok(());
    }
    if !matches!(ledger.subject_keyring, LayerStatus::Pending) {
        return Ok(());
    }

    // OLD subject-wrap cipher from the live (old) MEK; NEW from the ledger source.
    let old_dek = derive_subject_wrap_dek(load_mek(&enc_cfg.key_provider).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_dek = derive_subject_wrap_dek(load_mek(&ledger.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let old_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &old_dek));
    let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));

    crypto_shred
        .rewrap_keyring(old_cipher.as_ref(), &new_cipher, ledger.new_version)
        .map_err(|e| rotation_err(RotationError::KeyProvider(e.to_string())))?;
    mark_subject_keyring_complete(manager)?;

    // The subject keyring normally settles last, so it clears the rotation
    // ledger — BUT only when no other layer is still `Pending`. If a
    // cold+keyring rotation crashed and was reopened WITHOUT the cold tier
    // (cold stays `Pending`), clearing here would DROP the still-pending cold
    // layer, a regression from the pre-Slice-5 safe wedge. Gate the clear on
    // the OTHER layers being settled; `ledger` predates the `mark` above but
    // only its `subject_keyring` field changed, so its `cold`/`wal` fields are
    // still authoritative. If a layer is left pending, leave the ledger for
    // that layer's own finalize (or a reopen with the missing tier) to settle.
    if !matches!(ledger.cold, LayerStatus::Pending) && !matches!(ledger.wal, LayerStatus::Pending) {
        clear_rotation_state(manager);
    }
    Ok(())
}

/// Fail-closed guard for a crypto-shred subject-keyring re-wrap left `Pending`
/// by a crash, when THIS binary was built WITHOUT the crypto-shred
/// (`audit-export`) feature (Slice 5 review 4b).
///
/// The only finalizer that can complete a subject-keyring re-wrap
/// ([`finalize_resumed_subject_keyring_rotation`]) is `#[cfg(feature =
/// "audit-export")]`. So an `audit-export` binary that crashed mid-keyring
/// (ledger `subject_keyring = Pending`), then reopened by a NON-`audit-export`
/// binary, would have NO finalizer and NO diagnostic — the ledger would wedge
/// `Pending` forever. Mirror
/// [`AletheiaDB::fail_if_pending_enable_cold_without_tier`]: surface the stuck
/// state as an actionable `FailedPrecondition` instead of silent limbo. Wired
/// into the startup path (`db::config`) on the non-`audit-export` side. This
/// function is deliberately NOT feature-gated so it compiles and runs when
/// crypto-shred is absent.
///
/// A guarded no-op when no ledger is present, it is an `enable` ledger, or the
/// subject-keyring layer is not `Pending`.
///
/// # Errors
/// [`Error::FailedPrecondition`] when a forward/cancel rotation ledger records
/// `subject_keyring = Pending` and the crypto-shred feature is absent.
///
/// Gated to the non-`audit-export` build: when the crypto-shred feature IS
/// present, [`finalize_resumed_subject_keyring_rotation`] completes the re-wrap
/// instead, so this guard would be dead code.
#[cfg(not(feature = "audit-export"))]
pub(crate) fn fail_if_pending_subject_keyring_without_crypto_shred(
    manager: &IndexPersistenceManager,
) -> Result<()> {
    let Some(ledger) = read_rotation_state(manager)? else {
        return Ok(());
    };
    if matches!(ledger.direction, RotationDirection::Enable) {
        return Ok(());
    }
    if matches!(ledger.subject_keyring, LayerStatus::Pending) {
        return Err(Error::FailedPrecondition(
            "rotation ledger records a pending crypto-shred subject-keyring re-wrap, but this \
             binary was built without the crypto-shred / `audit-export` feature; reopen with the \
             crypto-shred / `audit-export` feature enabled to complete the subject-keyring re-wrap"
                .to_string(),
        ));
    }
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
    fn rotate_to_remote_source_fails_loud_and_writes_no_ledger() {
        // Issue #3620 (supersedes the #3602 finding-G refusal): rotating TO a
        // KMS/Vault source is no longer refused on the variant alone — the v3
        // ledger round-trips it. When the source is unavailable at rotation start
        // (Vault token env unset, KMS endpoint unreachable / not compiled), the
        // rotation fails LOUD — but crucially NOT with the old "not yet supported"
        // refusal — and writes NO breadcrumb (load_mek(new) fails before the
        // ledger write). Index-only DB so we reach run_rotation.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();

        let db = build_db_index_only(root, &old_key);
        seed(&db);

        // Vault with an unset token env: build_provider fails reading the token
        // BEFORE dialing — loud, no ledger, and NOT the old variant refusal.
        let vault_var = unique_var("VAULT_ROT_NOPE");
        // SAFETY: ensure absent.
        unsafe { std::env::remove_var(&vault_var) };
        let err = db
            .rotate_index_keys(KeyProviderConfig::Vault {
                address: "https://127.0.0.1:1".to_string(),
                token_env: vault_var,
                mount: "secret".to_string(),
                path: "app/mek".to_string(),
                key_field: "key".to_string(),
                namespace: None,
                ca_cert: None,
            })
            .unwrap_err();
        assert!(
            !err.to_string().contains("not yet supported"),
            "the old variant-only refusal must be gone, got: {err}"
        );
        assert!(!root.join("data").join("rotation.state").exists());

        // KMS at an unreachable endpoint (or not compiled) fails the same way.
        let err = db
            .rotate_index_keys(KeyProviderConfig::Kms {
                key_id: "alias/prod".to_string(),
                encrypted_data_key: "QUFBQQ==".to_string(),
                region: None,
                endpoint_url: Some("http://127.0.0.1:1".to_string()),
            })
            .unwrap_err();
        assert!(
            !err.to_string().contains("not yet supported"),
            "the old variant-only refusal must be gone, got: {err}"
        );
        assert!(!root.join("data").join("rotation.state").exists());
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
        let keyring = manager.keyring().unwrap();
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
                    false, // subject keyring not in scope
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
                    false, // subject keyring not in scope
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
        let keyring = manager.keyring().unwrap();
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

    // ── Issue #3680: cancel generalizes to arbitrary version transitions ──

    /// The pure version arithmetic: a `vN → vN+1` ledger resolves its old
    /// generation to `vN` for ANY N, and a degenerate `target_version` can never
    /// resolve below the base generation.
    #[test]
    fn rotation_old_version_generalizes_beyond_v1() {
        let source = KeyProviderConfig::File {
            path: "/nonexistent/new.key".into(),
        };
        for target in 2u32..=8 {
            let forward = super::RotationLedger::index_scope(
                RotationDirection::Forward,
                target,
                source.clone(),
            );
            assert_eq!(
                forward.rotation_old_version(),
                target - 1,
                "a v{} → v{target} rotation rolls back to v{}",
                target - 1,
                target - 1,
            );
            let cancel = super::RotationLedger::index_scope(
                RotationDirection::Cancel,
                target,
                source.clone(),
            );
            assert_eq!(cancel.rotation_old_version(), target - 1);
        }
        // Degenerate/legacy target versions floor at the base generation rather
        // than underflowing to a nonexistent one.
        for target in [0u32, 1] {
            let ledger = super::RotationLedger::index_scope(
                RotationDirection::Forward,
                target,
                source.clone(),
            );
            assert_eq!(ledger.rotation_old_version(), ENC_INDEX_KEY_VERSION_V1);
        }
    }

    /// A cancel of the SECOND rotation (`v2 → v3`) must roll back to `v2`, not to
    /// the base `v1`. Before Issue #3680 the old generation was hard-coded to the
    /// base version, so the un-migrated `v2` files matched no live keyring
    /// generation and the reverse pass could not put the dataset back.
    #[test]
    fn cancel_pending_rotation_rolls_back_to_the_ledgers_old_generation_not_v1() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key_a = root.join("a.key");
        let key_b = root.join("b.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key_a).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&key_b).unwrap();
        let indexes_dir = root.join("data").join("indexes");

        let db = build_db_index_only(root, &key_a);
        seed(&db);
        let manager = db.persistence_manager.clone().unwrap();
        let keyring = manager.keyring().unwrap();
        let cipher_a = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&key_a))
                .unwrap()
                .index_cipher(),
        );
        let cipher_b = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&key_b))
                .unwrap()
                .index_cipher(),
        );

        // Put the database in the state a FIRST completed rotation leaves behind:
        // every index file stamped v2, the keyring current at v2. (Re-stamped
        // under the same key A, which is what `enc_cfg` still names — exactly the
        // post-rotation, pre-provider-switch state the cancel path sees.)
        keyring.add_generation(2, std::sync::Arc::clone(&cipher_a));
        let to_v2 = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring.clone(),
            1,
            std::sync::Arc::clone(&cipher_a),
            2,
            std::sync::Arc::clone(&cipher_a),
        );
        to_v2.re_encrypt(&mut |_| true).unwrap();
        to_v2.complete().unwrap();
        assert_eq!(keyring.current_version(), 2);
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);

        // Now interrupt a SECOND rotation, v2 → v3, to key B: migrate some files
        // and leave the rest at v2, with a durable forward ledger.
        keyring.add_generation(3, std::sync::Arc::clone(&cipher_b));
        let to_v3 = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring.clone(),
            2,
            std::sync::Arc::clone(&cipher_a),
            3,
            std::sync::Arc::clone(&cipher_b),
        );
        let mut n = 0;
        to_v3
            .re_encrypt(&mut |_| {
                n += 1;
                n < 2
            })
            .unwrap();
        super::write_rotation_state(
            &manager,
            3,
            &KeyProviderConfig::File {
                path: key_b.clone(),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();

        let report = db.cancel_pending_rotation().expect("cancel must succeed");
        assert_eq!(report.old_version, 3, "cancel retires the v3 generation");
        assert_eq!(
            report.new_version, 2,
            "cancel rolls back to the ledger's old generation (v2), not the base v1",
        );
        assert!(!root.join("data").join("rotation.state").exists());
        // The whole dataset is uniformly back at v2 — no file stranded at v3, and
        // none wrongly re-stamped down to v1.
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
        assert_eq!(
            keyring.current_version(),
            2,
            "the new generation is retired from the live keyring",
        );
    }

    /// An enable/disable migration ledger rides the same file but is NOT a
    /// rotation — its `new_version` is not `old + 1`, so cancelling it as one
    /// would target a bogus generation. It is refused instead.
    #[test]
    fn cancel_pending_rotation_refuses_enable_and_disable_ledgers() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("k.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();
        let db = build_db_index_only(root, &key);
        seed(&db);
        let manager = db.persistence_manager.clone().unwrap();

        for direction in [
            super::RotationDirection::Enable,
            super::RotationDirection::Disable,
        ] {
            super::write_rotation_state(
                &manager,
                4,
                &KeyProviderConfig::File { path: key.clone() },
                direction,
            )
            .unwrap();
            let err = db.cancel_pending_rotation().unwrap_err();
            assert!(
                err.to_string().contains("no key rotation is in progress"),
                "a {direction:?} ledger must not be cancelled as a rotation, got: {err}",
            );
            // Refused, and the migration ledger is left intact for its own engine.
            assert!(root.join("data").join("rotation.state").exists());
        }
        super::clear_rotation_state(&manager);
    }

    // ── Issue #3783: cancel refuses while a non-index layer is re-keyed ──

    /// Snapshot every persisted index file's `AEIX` `key_version`, keyed by path —
    /// the observable that proves whether the reverse pass ran.
    fn index_version_map(indexes_dir: &Path) -> std::collections::BTreeMap<String, u32> {
        fn walk(dir: &Path, out: &mut std::collections::BTreeMap<String, u32>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if let Ok(bytes) = std::fs::read(&p)
                    && let Some(v) = index_file_key_version(&bytes)
                {
                    out.insert(p.to_string_lossy().into_owned(), v);
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(indexes_dir, &mut out);
        out
    }

    /// Plant a forward ledger with explicit per-layer statuses.
    fn plant_forward_ledger(
        manager: &std::sync::Arc<IndexPersistenceManager>,
        new_source: KeyProviderConfig,
        wal: LayerStatus,
        cold: LayerStatus,
        subject_keyring: LayerStatus,
    ) {
        super::write_ledger(
            manager,
            &super::RotationLedger {
                direction: super::RotationDirection::Forward,
                new_version: 2,
                new_source,
                index: LayerStatus::Complete,
                checkpoint: LayerStatus::Complete,
                wal,
                cold,
                wal_retire: LayerStatus::Skipped,
                mek_kcv: Some("00112233445566aa".to_string()),
                subject_keyring,
                algorithm: None,
            },
        )
        .unwrap();
    }

    /// The predicate the guard reads: only the three NON-index layers, and only
    /// when they are not `Skipped`, block a cancel. `index` / `checkpoint` (which
    /// the reverse pass DOES cover) and `wal_retire` (an enable/disable cleanup
    /// step naming no key generation) never block.
    #[test]
    fn unrollbackable_layers_names_only_rekeyed_non_index_layers() {
        let source = KeyProviderConfig::File {
            path: "/nonexistent/new.key".into(),
        };
        let base = super::RotationLedger::index_scope(
            super::RotationDirection::Forward,
            2,
            source.clone(),
        );
        // An index-scope ledger is fully rollbackable — this is the case the
        // reverse pass covers, so cancelling must stay available for it.
        assert!(base.unrollbackable_layers().is_empty());

        // Neither the index domain nor the enable/disable-only `wal_retire` step
        // blocks a cancel, whatever their status.
        let mut index_only = base.clone();
        index_only.index = LayerStatus::Complete;
        index_only.checkpoint = LayerStatus::Complete;
        index_only.wal_retire = LayerStatus::Pending;
        assert!(index_only.unrollbackable_layers().is_empty());

        // Each of the three layers blocks, whether Pending OR Complete: the ledger
        // cannot distinguish "never started" from "interrupted half-way".
        type SetLayer = fn(&mut super::RotationLedger, LayerStatus);
        let layers: [(&str, SetLayer); 3] = [
            ("wal", |l, s| l.wal = s),
            ("cold", |l, s| l.cold = s),
            ("subject_keyring", |l, s| l.subject_keyring = s),
        ];
        for status in [LayerStatus::Pending, LayerStatus::Complete] {
            for (name, set) in layers {
                let mut l = base.clone();
                set(&mut l, status);
                assert_eq!(
                    l.unrollbackable_layers(),
                    vec![format!("{name}={}", status.as_str())],
                    "{name}={status:?} must block a cancel",
                );
            }
        }

        // All three at once are named together, in driver order.
        let mut all = base.clone();
        all.wal = LayerStatus::Complete;
        all.cold = LayerStatus::Pending;
        all.subject_keyring = LayerStatus::Pending;
        assert_eq!(
            all.unrollbackable_layers().join(", "),
            "wal=complete, cold=pending, subject_keyring=pending",
        );
    }

    /// The cancel marker must PRESERVE the pending ledger's non-index layer
    /// statuses and its `mek_kcv`, not flatten them to `Skipped`/`None` the way the
    /// old `index_scope` marker did — otherwise the moment the cancel marker lands
    /// the ledger no longer records which layers were re-keyed, and the #3620
    /// wrong-key detection is lost across a cancel.
    #[test]
    fn cancel_scope_preserves_non_index_layer_statuses_and_kcv() {
        let pending = super::RotationLedger {
            direction: super::RotationDirection::Forward,
            new_version: 5,
            new_source: KeyProviderConfig::Env {
                variable: "MEK_NEW".to_string(),
            },
            index: LayerStatus::Complete,
            checkpoint: LayerStatus::Complete,
            wal: LayerStatus::Complete,
            cold: LayerStatus::Pending,
            wal_retire: LayerStatus::Skipped,
            mek_kcv: Some("deadbeefdeadbeef".to_string()),
            subject_keyring: LayerStatus::Pending,
            algorithm: None,
        };
        let cancel = super::RotationLedger::cancel_scope(&pending);

        assert_eq!(cancel.direction, super::RotationDirection::Cancel);
        assert_eq!(cancel.new_version, pending.new_version);
        assert_eq!(cancel.new_source, pending.new_source);
        // The reverse index/checkpoint pass still has to run.
        assert_eq!(cancel.index, LayerStatus::Pending);
        assert_eq!(cancel.checkpoint, LayerStatus::Pending);
        // Preserved verbatim — the record of what a cancel would have to undo.
        assert_eq!(cancel.wal, LayerStatus::Complete);
        assert_eq!(cancel.cold, LayerStatus::Pending);
        assert_eq!(cancel.subject_keyring, LayerStatus::Pending);
        assert_eq!(cancel.wal_retire, pending.wal_retire);
        // Preserved so #3620 wrong-key detection survives a cancel.
        assert_eq!(cancel.mek_kcv.as_deref(), Some("deadbeefdeadbeef"));
        assert_eq!(
            cancel.rotation_old_version(),
            4,
            "the generation to roll back to is unchanged by the direction flip",
        );

        // And every preserved field survives the durable round-trip.
        let read = ledger_roundtrip(&cancel);
        assert_eq!(read, cancel);
    }

    /// A cancel must be REFUSED — loudly, with the offending layers named, and
    /// with nothing touched — when the interrupted forward rotation already moved
    /// the WAL / cold tier / subject keyring onto the new key. Previously it ran
    /// the index reverse pass, cleared the ledger, and returned `Ok`, telling the
    /// operator the new key was never adopted while those layers were still on it.
    #[test]
    fn cancel_pending_rotation_refuses_when_a_non_index_layer_was_rekeyed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");
        let ledger_path = root.join("data").join("rotation.state");

        let db = build_db_index_only(root, &old_key);
        seed(&db);
        let manager = db.persistence_manager.clone().unwrap();
        let source = KeyProviderConfig::File {
            path: new_key.clone(),
        };

        // Half-migrate the index tree to v2, exactly as an interrupted forward
        // pass would leave it, so a reverse pass running anyway is observable.
        let keyring = manager.keyring().unwrap();
        let cipher_new = std::sync::Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig::file_based(&new_key))
                .unwrap()
                .index_cipher(),
        );
        let cipher_old = keyring.current_cipher().unwrap();
        keyring.add_generation(2, std::sync::Arc::clone(&cipher_new));
        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring.clone(),
            1,
            cipher_old,
            2,
            std::sync::Arc::clone(&cipher_new),
        );
        let mut n = 0;
        engine
            .re_encrypt(&mut |_| {
                n += 1;
                n < 2
            })
            .unwrap();
        let before_files = index_version_map(&indexes_dir);
        assert!(
            before_files.values().any(|v| *v == 2) && before_files.values().any(|v| *v == 1),
            "test setup must leave a genuinely half-migrated tree: {before_files:?}",
        );

        for (wal, cold, subject_keyring, expect) in [
            (
                LayerStatus::Complete,
                LayerStatus::Skipped,
                LayerStatus::Skipped,
                "wal=complete",
            ),
            (
                LayerStatus::Pending,
                LayerStatus::Skipped,
                LayerStatus::Skipped,
                "wal=pending",
            ),
            (
                LayerStatus::Skipped,
                LayerStatus::Complete,
                LayerStatus::Skipped,
                "cold=complete",
            ),
            (
                LayerStatus::Skipped,
                LayerStatus::Skipped,
                LayerStatus::Pending,
                "subject_keyring=pending",
            ),
            (
                LayerStatus::Complete,
                LayerStatus::Pending,
                LayerStatus::Pending,
                "wal=complete, cold=pending, subject_keyring=pending",
            ),
        ] {
            plant_forward_ledger(&manager, source.clone(), wal, cold, subject_keyring);
            let before_ledger = std::fs::read(&ledger_path).unwrap();

            let err = db.cancel_pending_rotation().unwrap_err();
            assert!(
                matches!(err, Error::FailedPrecondition(_)),
                "a refused cancel is an operator precondition failure, got: {err:?}",
            );
            let msg = err.to_string();
            assert!(
                msg.contains("cannot cancel this key rotation") && msg.contains(expect),
                "the refusal must name the blocking layer(s) ({expect}), got: {msg}",
            );
            assert!(
                msg.contains("--resume"),
                "the refusal must point at rolling forward, got: {msg}",
            );

            // Nothing was touched: the pending FORWARD ledger is byte-identical
            // (never flipped to a cancel marker) and the half-migrated index tree
            // is exactly as it was, so `--resume` can still finish the rotation.
            assert_eq!(
                std::fs::read(&ledger_path).unwrap(),
                before_ledger,
                "a refused cancel must leave the pending forward ledger untouched",
            );
            assert_eq!(
                index_version_map(&indexes_dir),
                before_files,
                "a refused cancel must not run the index reverse pass",
            );
        }

        // Flatten the blocking layers back to `Skipped` and the same ledger
        // cancels normally — the refusal is scoped to what the pass cannot undo.
        plant_forward_ledger(
            &manager,
            source,
            LayerStatus::Skipped,
            LayerStatus::Skipped,
            LayerStatus::Skipped,
        );
        let report = db
            .cancel_pending_rotation()
            .expect("an index-only rotation must still cancel");
        assert_eq!(report.new_version, 1);
        assert!(!ledger_path.exists());
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);
    }

    /// Resuming an INTERRUPTED cancel is refused the same way, checked before any
    /// keyring generation is installed and before the WAL block that would
    /// otherwise roll the WAL *forward* off a cancel ledger. The ledger is
    /// retained (never cleared), so nothing reports success.
    #[test]
    fn resume_refuses_a_cancel_ledger_recording_a_rekeyed_layer() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
        let ledger_path = root.join("data").join("rotation.state");

        let db = build_db_index_only(root, &old_key);
        seed(&db);
        let manager = db.persistence_manager.clone().unwrap();

        super::write_ledger(
            &manager,
            &super::RotationLedger {
                direction: super::RotationDirection::Cancel,
                new_version: 2,
                new_source: KeyProviderConfig::File {
                    path: new_key.clone(),
                },
                index: LayerStatus::Pending,
                checkpoint: LayerStatus::Pending,
                wal: LayerStatus::Complete,
                cold: LayerStatus::Pending,
                wal_retire: LayerStatus::Skipped,
                mek_kcv: None,
                subject_keyring: LayerStatus::Skipped,
                algorithm: None,
            },
        )
        .unwrap();
        let before = std::fs::read(&ledger_path).unwrap();

        let err = db.resume_pending_index_rotation().unwrap_err();
        assert!(
            matches!(err, Error::FailedPrecondition(_)),
            "expected the cancel refusal, got: {err:?}",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("wal=complete") && msg.contains("cold=pending"),
            "the refusal must name every blocking layer, got: {msg}",
        );
        assert_eq!(
            std::fs::read(&ledger_path).unwrap(),
            before,
            "a refused cancel-resume must retain the ledger verbatim",
        );

        super::clear_rotation_state(&manager);
    }

    /// The refusal is audited as a FAILURE (with a key-safe category), never as a
    /// completed rotation — the operator must not be able to read
    /// `key.rotation.completed` out of the log and conclude the rollback happened.
    #[test]
    fn refused_cancel_audits_a_failure_not_a_completion() {
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
            instance_id: Some("node-3783".into()),
            ..AuditConfig::default()
        };

        let db = build_db_index_only_with_enc(root, enc);
        seed(&db);
        let manager = db.persistence_manager.clone().unwrap();
        plant_forward_ledger(
            &manager,
            KeyProviderConfig::File { path: new_key },
            LayerStatus::Complete,
            LayerStatus::Skipped,
            LayerStatus::Skipped,
        );

        db.cancel_pending_rotation().unwrap_err();

        let log = std::fs::read_to_string(&audit_path).expect("audit log written");
        assert!(
            log.contains("key.rotation.failed"),
            "expected a rotation.failed event: {log}"
        );
        assert!(
            !log.contains("key.rotation.completed"),
            "a refused cancel must NEVER audit a completed rotation: {log}"
        );
        // The audit category is the opaque token, never the raw error text (P2.2).
        assert!(
            !log.contains("split-key"),
            "the audit event must not carry the raw error message: {log}"
        );
        super::clear_rotation_state(&manager);
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
            wal_retire: super::LayerStatus::Skipped,
            mek_kcv: None,
            subject_keyring: super::LayerStatus::Skipped,
            algorithm: None,
        };
        super::write_ledger(&manager, &ledger).unwrap();

        let read = super::read_rotation_state(&manager)
            .unwrap()
            .expect("ledger present");
        assert_eq!(read, ledger, "every ledger field must round-trip");

        let body = std::fs::read_to_string(root.join("data").join("rotation.state")).unwrap();
        // Since #3620 a serde build emits the v3 format (full JSON source).
        assert!(body.contains("version=3"), "expected v3 format: {body}");
        assert!(body.contains("target_version=3"));
        assert!(body.contains("layer.index=complete"));
        assert!(body.contains("layer.cold=pending"));
        // Source REFERENCE only (env var NAME), never key bytes.
        assert!(
            body.contains("new_source_json=") && body.contains("MEK_VAR"),
            "v3 serializes the full config as JSON with the env NAME: {body}"
        );

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

    /// Slice 5 review 4b (FIX 3): when THIS binary lacks the crypto-shred
    /// (`audit-export`) feature there is no finalizer for a subject-keyring
    /// re-wrap left `Pending` by a crash, so the startup guard must fail LOUDLY
    /// (a `FailedPrecondition`) rather than wedge the ledger `Pending` forever.
    /// A `Complete`/`Skipped` keyring layer — and an `enable` ledger — pass
    /// cleanly. Only meaningful on the non-`audit-export` build (the guard fn is
    /// gated to it).
    #[cfg(not(feature = "audit-export"))]
    #[test]
    fn pending_subject_keyring_without_crypto_shred_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            None,
        ));

        let mut ledger = super::RotationLedger {
            direction: super::RotationDirection::Forward,
            new_version: 2,
            new_source: KeyProviderConfig::Env {
                variable: "MEK_VAR".to_string(),
            },
            index: super::LayerStatus::Skipped,
            checkpoint: super::LayerStatus::Skipped,
            wal: super::LayerStatus::Skipped,
            cold: super::LayerStatus::Skipped,
            wal_retire: super::LayerStatus::Skipped,
            mek_kcv: None,
            subject_keyring: super::LayerStatus::Pending,
            algorithm: None,
        };
        super::write_ledger(&manager, &ledger).unwrap();

        // A `Pending` subject-keyring layer + no crypto-shred feature -> loud
        // FailedPrecondition with actionable remediation.
        let err =
            super::fail_if_pending_subject_keyring_without_crypto_shred(&manager).unwrap_err();
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "expected FailedPrecondition, got: {err:?}"
        );
        assert!(
            err.to_string().contains("audit-export"),
            "message must name the feature to reopen with: {err}"
        );

        // No pending re-wrap (Complete) -> clean pass, no wedge.
        ledger.subject_keyring = super::LayerStatus::Complete;
        super::write_ledger(&manager, &ledger).unwrap();
        super::fail_if_pending_subject_keyring_without_crypto_shred(&manager).unwrap();

        // An `enable` ledger is never a rotation re-wrap -> clean pass.
        ledger.direction = super::RotationDirection::Enable;
        ledger.subject_keyring = super::LayerStatus::Pending;
        super::write_ledger(&manager, &ledger).unwrap();
        super::fail_if_pending_subject_keyring_without_crypto_shred(&manager).unwrap();

        // No ledger at all -> clean pass.
        super::clear_rotation_state(&manager);
        super::fail_if_pending_subject_keyring_without_crypto_shred(&manager).unwrap();
    }

    // ── #3616 PR4: encrypted → plaintext DISABLE direction (mirror of Enable) ─

    #[test]
    fn disable_scope_ledger_round_trips_by_name() {
        // The encrypted → plaintext DISABLE plan (Issue #3616 PR4) is recorded on
        // the SAME v2 cross-layer ledger as a rotation/enable, discriminated by
        // `direction=disable`. Every per-layer status is addressed BY NAME (no
        // positional/count assumption), so a future layer added the same by-name
        // way (e.g. GDPR `subject_keyring`) composes without editing this path.
        //
        // Semantics are the inverse of `enable_scope`: the WAL is ALWAYS `Pending`
        // (a disable's whole point is to strip the live WAL's encryption); index /
        // checkpoint / cold are `Pending` only when there are encrypted bytes of
        // that layer to unwrap, else `Skipped`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            None,
        ));

        // Retiring the CURRENT key (version 7); the recorded source is the current
        // key needed to build the DECRYPT cipher during a resumed disable.
        let ledger = super::RotationLedger::disable_scope(
            7,
            KeyProviderConfig::Env {
                variable: "CURRENT_MEK".to_string(),
            },
            /* index_in_scope */ true,
            /* cold_in_scope */ false,
        );
        assert_eq!(ledger.direction, super::RotationDirection::Disable);
        assert_eq!(
            ledger.wal,
            super::LayerStatus::Pending,
            "WAL always in scope"
        );
        assert_eq!(ledger.index, super::LayerStatus::Pending);
        assert_eq!(ledger.checkpoint, super::LayerStatus::Pending);
        assert_eq!(ledger.cold, super::LayerStatus::Skipped, "no cold tier");
        assert_eq!(ledger.wal_retire, super::LayerStatus::Pending);

        super::write_ledger(&manager, &ledger).unwrap();

        let read = super::read_rotation_state(&manager)
            .unwrap()
            .expect("ledger present");
        assert_eq!(read, ledger, "every disable-ledger field must round-trip");

        let body = std::fs::read_to_string(root.join("data").join("rotation.state")).unwrap();
        // Since #3620 a serde build emits the v3 format (full JSON source).
        assert!(body.contains("version=3"), "expected v3 format: {body}");
        assert!(body.contains("direction=disable"), "direction: {body}");
        assert!(body.contains("layer.wal=pending"));
        assert!(body.contains("layer.index=pending"));
        assert!(body.contains("layer.cold=skipped"));
        // Source REFERENCE only (env var NAME), never key bytes.
        assert!(
            body.contains("new_source_json=") && body.contains("CURRENT_MEK"),
            "v3 serializes the full config as JSON with the env NAME: {body}"
        );
    }

    #[test]
    fn disable_ledger_provisions_current_version_not_minus_one() {
        // Crash-consistency regression (gemini HIGH): after a mid-disable crash the
        // on-disk ciphertext is still stamped with the CURRENT key version, which a
        // `disable_scope` ledger records verbatim as `new_version` (NOT `old + 1`).
        // `resolve_provisioned_key_version` must therefore provision the keyring to
        // `new_version` as-is for a disable ledger — the old uniform `new_version - 1`
        // would provision `current - 1` and leave startup unable to decrypt files
        // stamped `current`.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let indexes_dir = root.join("indexes");
        let wal_dir = root.join("wal");

        // Disable case: pending disable ledger recording current key version 7.
        let disable_dir = root.join("disable_data");
        let disable_mgr = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            disable_dir.clone(),
            None,
        ));
        let disable_ledger = super::RotationLedger::disable_scope(
            7,
            KeyProviderConfig::File {
                path: root.join("mek.key"),
            },
            /* index_in_scope */ true,
            /* cold_in_scope */ false,
        );
        super::write_ledger(&disable_mgr, &disable_ledger).unwrap();
        let resolved =
            super::resolve_provisioned_key_version(&indexes_dir, &wal_dir, disable_mgr.base_path())
                .unwrap();
        assert_eq!(
            resolved, 7,
            "a disable ledger must provision to its recorded current version (7), not 7 - 1"
        );

        // Regression guard: the additive change must NOT alter the Forward path —
        // a forward ledger with `new_version = old + 1` still resolves to `old`.
        let fwd_dir = root.join("forward_data");
        let fwd_mgr =
            std::sync::Arc::new(IndexPersistenceManager::with_cipher(fwd_dir.clone(), None));
        super::write_rotation_state(
            &fwd_mgr,
            6, // new_version = old(5) + 1
            &KeyProviderConfig::File {
                path: root.join("new.key"),
            },
            super::RotationDirection::Forward,
        )
        .unwrap();
        let fwd_resolved =
            super::resolve_provisioned_key_version(&indexes_dir, &wal_dir, fwd_mgr.base_path())
                .unwrap();
        assert_eq!(
            fwd_resolved, 5,
            "a forward ledger (new_version = old + 1) must still resolve to old (5)"
        );
    }

    #[test]
    fn disable_ledger_is_skipped_by_rotation_resume() {
        // A `direction=disable` ledger must NEVER be driven by the rotation resume
        // paths (which assume an already-encrypted old→new generation). It is the
        // disable engine's own resume that completes it — exactly as an `enable`
        // ledger is skipped. Proven by driving the ACTUAL rotation resume entry
        // point (`resume_pending_rotation`) against a pending disable ledger and
        // asserting it returns `Ok(None)` (skipped, breadcrumb untouched) rather
        // than attempting to resume it as an old→new-generation rotation.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let key = root.join("mek.key");
        crate::encryption::FileKeyProvider::generate_key_file(&key).unwrap();

        let enc_cfg = EncryptionConfig::file_based(&key);
        // A disable ledger only ever exists on an encrypted DB, so give the
        // manager the real index cipher — a faithful encrypted-startup shape.
        let manager = std::sync::Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(std::sync::Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let ledger = super::RotationLedger::disable_scope(
            3,
            KeyProviderConfig::File { path: key.clone() },
            true,
            false,
        );
        super::write_ledger(&manager, &ledger).unwrap();

        // The real rotation resume entry point must SKIP a disable ledger.
        let report = super::resume_pending_rotation(&manager, &enc_cfg, None, None)
            .expect("resume must not error on a disable ledger");
        assert!(
            report.is_none(),
            "rotation resume must skip a disable ledger (Ok(None)), never drive it"
        );

        // And it must leave the breadcrumb in place for the disable engine's own
        // resume to complete — skipping must not clear or mutate the ledger.
        let read = super::read_rotation_state(&manager)
            .unwrap()
            .expect("disable ledger must survive a skipping rotation-resume pass");
        assert!(
            matches!(read.direction, super::RotationDirection::Disable),
            "a disable ledger must parse back as Disable, never a rotation"
        );
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
            body.contains("version=3") && body.contains("layer.index=pending"),
            "expected a v3 ledger: {body}"
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
        let keyring = manager.keyring().unwrap();
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
            body.contains("version=3") && body.contains("layer.index=pending"),
            "expected a v3 ledger on disk: {body}"
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
        let keyring = manager.keyring().unwrap();
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
        let keyring = manager.keyring().unwrap();
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
                wal_retire: super::LayerStatus::Skipped,
                mek_kcv: None,
                subject_keyring: super::LayerStatus::Skipped,
                algorithm: None,
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
                wal_retire: super::LayerStatus::Skipped,
                mek_kcv: None,
                subject_keyring: super::LayerStatus::Skipped,
                algorithm: None,
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
        // A ledger with an UNKNOWN format version (here v4) must fail closed as
        // corrupt/unsupported, never be silently accepted — v1, v2 and v3 (#3620)
        // are the only shapes this reader understands. The rest of the ledger is
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
            b"version=4\ndirection=forward\ntarget_version=4\nnew_source_kind=file\nnew_source_value=/etc/aletheia/new.key\nlayer.index=pending\n",
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
                    false, // subject keyring not in scope
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
                    false, // subject keyring not in scope
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

    // ── Issue #3620: durable rotation to passphrase/KMS/Vault key sources ──────
    //
    // The `version=3` ledger serializes the FULL (non-secret) KeyProviderConfig
    // via serde_json so remote/derived sources round-trip and crash-resume, plus
    // a MEK key-check value (KCV) for precise wrong-key detection at resume.

    /// Process-unique env-var name for parallel-test safety.
    fn unique_var(tag: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        format!(
            "ALETHEIADB_TEST_ROT3620_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Build a ledger with explicit fields for serialization round-trip tests.
    fn mk_ledger(
        direction: RotationDirection,
        new_source: KeyProviderConfig,
        kcv: Option<String>,
    ) -> RotationLedger {
        RotationLedger {
            direction,
            new_version: 2,
            new_source,
            index: LayerStatus::Pending,
            checkpoint: LayerStatus::Pending,
            wal: LayerStatus::Skipped,
            cold: LayerStatus::Skipped,
            wal_retire: LayerStatus::Skipped,
            mek_kcv: kcv,
            subject_keyring: LayerStatus::Skipped,
            algorithm: None,
        }
    }

    /// Serialize `ledger` to a temp `rotation.state` and read it straight back.
    fn ledger_roundtrip(ledger: &RotationLedger) -> RotationLedger {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        let body = super::serialize_ledger(ledger).unwrap();
        std::fs::write(&path, body.as_bytes()).unwrap();
        super::read_rotation_state_at(&path)
            .unwrap()
            .expect("ledger must parse back")
    }

    #[test]
    fn ledger_v3_roundtrips_kms_config() {
        // A KMS source with all four fields, incl. a base64 blob with `=` padding
        // (exercises the reader's split_once('=') robustness).
        let src = KeyProviderConfig::Kms {
            key_id: "alias/prod-mek".to_string(),
            encrypted_data_key: "QUJDRA==".to_string(), // base64 with padding
            region: Some("eu-west-1".to_string()),
            endpoint_url: Some("https://kms.eu-west-1.amazonaws.com".to_string()),
        };
        let ledger = mk_ledger(RotationDirection::Forward, src.clone(), None);
        let got = ledger_roundtrip(&ledger);
        assert_eq!(got, ledger);
        assert_eq!(got.new_source, src);
    }

    #[test]
    fn ledger_v3_roundtrips_vault_config() {
        // All seven Vault fields, incl. namespace: None and a ca_cert path.
        let src = KeyProviderConfig::Vault {
            address: "https://vault.example.com:8200".to_string(),
            token_env: "VAULT_TOKEN".to_string(),
            mount: "secret".to_string(),
            path: "mek/aletheia".to_string(),
            key_field: "key".to_string(),
            namespace: None,
            ca_cert: Some("/etc/ssl/vault-ca.pem".into()),
        };
        let ledger = mk_ledger(RotationDirection::Disable, src.clone(), None);
        let got = ledger_roundtrip(&ledger);
        assert_eq!(got, ledger);
        assert_eq!(got.new_source, src);
    }

    #[test]
    fn ledger_v3_roundtrips_passphrase_config() {
        let src = KeyProviderConfig::PassphraseFile {
            path: "/keys/mek.aekf".into(),
            passphrase_env: "MEK_PASSPHRASE".to_string(),
        };
        let ledger = mk_ledger(RotationDirection::Enable, src.clone(), None);
        let got = ledger_roundtrip(&ledger);
        assert_eq!(got, ledger);
    }

    #[test]
    fn ledger_v3_roundtrips_file_and_env() {
        for src in [
            KeyProviderConfig::File {
                path: "/keys/mek.key".into(),
            },
            KeyProviderConfig::Env {
                variable: "ALETHEIADB_MEK".to_string(),
            },
        ] {
            let ledger = mk_ledger(RotationDirection::Forward, src.clone(), None);
            assert_eq!(ledger_roundtrip(&ledger), ledger);
        }
    }

    #[test]
    fn ledger_v3_roundtrips_with_kcv() {
        let src = KeyProviderConfig::File {
            path: "/k.key".into(),
        };
        let ledger = mk_ledger(
            RotationDirection::Forward,
            src,
            Some("0123456789abcdef".to_string()),
        );
        let got = ledger_roundtrip(&ledger);
        assert_eq!(got.mek_kcv.as_deref(), Some("0123456789abcdef"));
        assert_eq!(got, ledger);
    }

    /// Issue #3708: an enable ledger's pinned CONCRETE algorithm round-trips through
    /// serialize→parse, `enable_scope` resolves `Auto` to a concrete variant (never
    /// persisting `Auto`), and a legacy ledger with no `algorithm=` line loads
    /// `None`. `verify_resumed_enable_algorithm` then accepts a matching config and
    /// refuses a mismatch or a `None` (legacy) pin.
    #[test]
    fn enable_ledger_pins_and_roundtrips_concrete_algorithm() {
        use crate::encryption::factory::Algorithm;
        let src = KeyProviderConfig::File {
            path: "/k.key".into(),
        };

        // `enable_scope` resolves Auto → a concrete variant (never `Auto`).
        let pinned = super::RotationLedger::enable_scope(
            super::ENABLE_KEY_VERSION,
            src.clone(),
            true,
            false,
            Algorithm::Auto,
        );
        let concrete = Algorithm::Auto.resolve();
        assert_eq!(pinned.algorithm, Some(concrete));
        assert_ne!(pinned.algorithm, Some(Algorithm::Auto));

        // Round-trips through the on-disk ledger format.
        let got = ledger_roundtrip(&pinned);
        assert_eq!(got.algorithm, Some(concrete));
        assert_eq!(got, pinned);
        let body = super::serialize_ledger(&pinned).unwrap();
        assert!(
            body.contains("algorithm="),
            "enable ledger emits the pin: {body}"
        );

        // A rotation (non-enable) ledger carries NO algorithm pin.
        let rot = mk_ledger(RotationDirection::Forward, src.clone(), None);
        assert_eq!(rot.algorithm, None);
        assert!(
            !super::serialize_ledger(&rot)
                .unwrap()
                .contains("algorithm=")
        );

        // Legacy ledger with no `algorithm=` line loads `None`.
        assert_eq!(ledger_roundtrip(&rot).algorithm, None);

        // The verifier: match → Ok(concrete); mismatch → Err; legacy `None` → Err.
        let other = if concrete == Algorithm::Aes256Gcm {
            Algorithm::ChaCha20Poly1305
        } else {
            Algorithm::Aes256Gcm
        };
        assert_eq!(
            super::verify_resumed_enable_algorithm(Some(concrete), Algorithm::Auto).unwrap(),
            concrete
        );
        assert!(super::verify_resumed_enable_algorithm(Some(concrete), other).is_err());
        assert!(super::verify_resumed_enable_algorithm(None, Algorithm::Auto).is_err());
    }

    #[test]
    fn ledger_v3_is_version_3_and_uses_json_source() {
        let src = KeyProviderConfig::Kms {
            key_id: "alias/k".to_string(),
            encrypted_data_key: "QQ==".to_string(),
            region: None,
            endpoint_url: None,
        };
        let body =
            super::serialize_ledger(&mk_ledger(RotationDirection::Forward, src, None)).unwrap();
        assert!(
            body.starts_with("version=3\n"),
            "must emit version=3: {body}"
        );
        assert!(
            body.contains("new_source_json="),
            "must use JSON source: {body}"
        );
        assert!(
            !body.contains("new_source_kind="),
            "v3 must NOT emit the legacy two-string form: {body}"
        );
        // Single-line JSON: exactly one `new_source_json=` line, no stray newline.
        assert_eq!(body.matches("new_source_json=").count(), 1);
    }

    #[test]
    fn ledger_v3_never_contains_plaintext_secret() {
        // The env-var NAMES appear in the ledger; the SECRET VALUES they point at
        // must NEVER be serialized (the config holds names, not secrets).
        let pass_var = unique_var("SECRET_PASS");
        let token_var = unique_var("SECRET_TOKEN");
        let sentinel_pass = "SUPER-SECRET-PASSPHRASE-VALUE-8f2a";
        let sentinel_token = "hvs.SUPER-SECRET-VAULT-TOKEN-9c1b";
        // SAFETY: test-only, unique names.
        unsafe {
            std::env::set_var(&pass_var, sentinel_pass);
            std::env::set_var(&token_var, sentinel_token);
        }

        for src in [
            KeyProviderConfig::PassphraseFile {
                path: "/keys/mek.aekf".into(),
                passphrase_env: pass_var.clone(),
            },
            KeyProviderConfig::Vault {
                address: "https://vault:8200".to_string(),
                token_env: token_var.clone(),
                mount: "secret".to_string(),
                path: "mek".to_string(),
                key_field: "key".to_string(),
                namespace: None,
                ca_cert: None,
            },
        ] {
            let body =
                super::serialize_ledger(&mk_ledger(RotationDirection::Forward, src, None)).unwrap();
            assert!(
                !body.contains(sentinel_pass) && !body.contains(sentinel_token),
                "a plaintext secret leaked into the ledger bytes: {body}"
            );
        }
        // The var NAME is present (it is not a secret).
        let body = super::serialize_ledger(&mk_ledger(
            RotationDirection::Forward,
            KeyProviderConfig::PassphraseFile {
                path: "/k.aekf".into(),
                passphrase_env: pass_var.clone(),
            },
            None,
        ))
        .unwrap();
        assert!(body.contains(&pass_var), "the env-var NAME should appear");

        // SAFETY: test-only cleanup.
        unsafe {
            std::env::remove_var(&pass_var);
            std::env::remove_var(&token_var);
        }
    }

    #[test]
    fn ledger_v1_still_parses() {
        // A legacy #488 v1 breadcrumb: no layer.* lines, `new_version`, two-string
        // File source. Must resume its index pass (index Pending, rest Skipped).
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=1\ndirection=forward\nnew_version=2\nnew_source_kind=file\nnew_source_value=/keys/new.key\n",
        )
        .unwrap();
        let got = super::read_rotation_state_at(&path).unwrap().unwrap();
        assert_eq!(
            got.new_source,
            KeyProviderConfig::File {
                path: "/keys/new.key".into()
            }
        );
        assert_eq!(got.index, LayerStatus::Pending);
        assert_eq!(got.checkpoint, LayerStatus::Skipped);
        assert_eq!(got.wal, LayerStatus::Skipped);
        assert_eq!(got.cold, LayerStatus::Skipped);
        assert_eq!(got.mek_kcv, None);
    }

    #[test]
    fn ledger_v2_still_parses() {
        // A v2 cross-layer ledger with the two-string Env source and layer lines.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=2\ndirection=enable\ntarget_version=1\nnew_source_kind=env\nnew_source_value=ALETHEIADB_MEK\nlayer.index=complete\nlayer.checkpoint=complete\nlayer.wal=pending\nlayer.cold=skipped\nlayer.wal_retire=pending\n",
        )
        .unwrap();
        let got = super::read_rotation_state_at(&path).unwrap().unwrap();
        assert_eq!(
            got.new_source,
            KeyProviderConfig::Env {
                variable: "ALETHEIADB_MEK".to_string()
            }
        );
        assert_eq!(got.direction, RotationDirection::Enable);
        assert_eq!(got.index, LayerStatus::Complete);
        assert_eq!(got.wal, LayerStatus::Pending);
        assert_eq!(got.cold, LayerStatus::Skipped);
        assert_eq!(got.wal_retire, LayerStatus::Pending);
        assert_eq!(got.mek_kcv, None);
    }

    #[test]
    fn ledger_v3_corrupt_json_fails_closed() {
        // A present-but-garbled JSON source must fail closed (InconsistentState),
        // never be silently treated as "no rotation".
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=3\ndirection=forward\ntarget_version=2\nnew_source_json={\"type\":\"kms\",\"key_id\":\nlayer.index=pending\n",
        )
        .unwrap();
        let err = super::read_rotation_state_at(&path).unwrap_err();
        assert!(
            matches!(err, Error::Storage(StorageError::InconsistentState { .. })),
            "corrupt v3 JSON must fail closed, got: {err:?}"
        );
    }

    #[test]
    fn ledger_v3_unknown_version_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=9\ndirection=forward\ntarget_version=2\nnew_source_json={\"type\":\"file\",\"path\":\"/k\"}\n",
        )
        .unwrap();
        assert!(super::read_rotation_state_at(&path).is_err());
    }

    #[test]
    fn ledger_v3_prefers_json_over_leftover_v2_two_string() {
        // Issue #3620 (FIX 4): a v3 ledger carrying BOTH the `new_source_json` line
        // AND leftover v2 `new_source_kind`/`new_source_value` lines must prefer the
        // JSON source (the reader's documented precedence), never the stale
        // two-string reference.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=3\ndirection=forward\ntarget_version=2\nnew_source_json={\"type\":\"file\",\"path\":\"/from/json.key\"}\nnew_source_kind=env\nnew_source_value=STALE_V2_VAR\nlayer.index=pending\n",
        )
        .unwrap();
        let got = super::read_rotation_state_at(&path).unwrap().unwrap();
        assert_eq!(
            got.new_source,
            KeyProviderConfig::File {
                path: "/from/json.key".into()
            },
            "the v3 JSON source must win over leftover v2 two-string lines"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_durable_creates_owner_only_0600_file() {
        // Issue #3620 (FIX 2): rotation.state / encryption.state both funnel through
        // `write_durable`; the file must be owner-only (0600), never world-readable
        // (it carries a CMK-useless KMS blob + the KCV). Matches the auth keys.json
        // 0600 precedent.
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        super::write_durable(&path, b"version=3\ndirection=forward\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {mode:o}");
    }

    #[test]
    fn ledger_v3_tolerates_extra_subject_keyring_layer_line() {
        // Forward-compat with slice 5 (#3721), which adds a by-name
        // `layer.subject_keyring` field to the ledger. A v3 ledger carrying BOTH my
        // #3620 additions (new_source_json + mek_kcv) AND an (unknown-to-this-build)
        // `layer.subject_keyring=pending` line must still parse: the new_source/KCV
        // and known layers reconstruct correctly and the unknown line is ignored
        // gracefully (not fail-closed). By-line/by-name handling makes this compose.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("rotation.state");
        std::fs::write(
            &path,
            "version=3\ndirection=forward\ntarget_version=2\nnew_source_json={\"type\":\"vault\",\"address\":\"https://v:8200\",\"token_env\":\"VT\",\"mount\":\"secret\",\"path\":\"mek\",\"key_field\":\"key\",\"namespace\":null,\"ca_cert\":null}\nmek_kcv=00112233aabbccdd\nlayer.index=pending\nlayer.checkpoint=pending\nlayer.wal=complete\nlayer.cold=skipped\nlayer.wal_retire=skipped\nlayer.subject_keyring=pending\n",
        )
        .unwrap();
        let got = super::read_rotation_state_at(&path).unwrap().unwrap();
        assert!(matches!(got.new_source, KeyProviderConfig::Vault { .. }));
        assert_eq!(got.mek_kcv.as_deref(), Some("00112233aabbccdd"));
        assert_eq!(got.wal, LayerStatus::Complete);
        assert_eq!(got.cold, LayerStatus::Skipped);
    }

    #[test]
    fn kcv_is_deterministic_and_key_dependent() {
        let mek_a = Zeroizing::new([7u8; 32]);
        let mut b = [7u8; 32];
        b[0] = 8;
        let mek_b = Zeroizing::new(b);
        let kcv_a1 = super::compute_mek_kcv(&mek_a).unwrap();
        let kcv_a2 = super::compute_mek_kcv(&mek_a).unwrap();
        let kcv_b = super::compute_mek_kcv(&mek_b).unwrap();
        assert_eq!(kcv_a1, kcv_a2, "KCV must be deterministic for one MEK");
        assert_ne!(kcv_a1, kcv_b, "different MEKs must yield different KCVs");
        assert_eq!(kcv_a1.len(), 16, "8-byte KCV rendered as 16 hex chars");
        // The KCV must not equal any prefix of the raw key material (one-way).
        assert!(!kcv_a1.contains("07070707"));
    }

    // ── Crash-resume through build_provider (PassphraseFile is fully local) ────

    /// Build a `version=3` ledger + partial re-encrypt to simulate a crash
    /// mid-rotation to `pp_source` (whose MEK is `mek_new`), then return the root.
    fn passphrase_source(
        dir: &Path,
        passphrase: &str,
        var: &str,
    ) -> (KeyProviderConfig, Zeroizing<[u8; 32]>) {
        let pp_path = dir.join("mek.aekf");
        let mek =
            crate::encryption::generate_passphrase_key_file(&pp_path, passphrase, true).unwrap();
        (
            KeyProviderConfig::PassphraseFile {
                path: pp_path,
                passphrase_env: var.to_string(),
            },
            mek,
        )
    }

    fn index_cipher_for_source(src: &KeyProviderConfig) -> Arc<dyn Cipher> {
        Arc::clone(
            EncryptionManager::from_config(&EncryptionConfig {
                enabled: true,
                algorithm: Algorithm::default(),
                key_provider: src.clone(),
                ..Default::default()
            })
            .unwrap()
            .index_cipher(),
        )
    }

    #[test]
    fn crash_resume_to_passphrase_source_reunwraps_aekf() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let var = unique_var("RESUME_PP");
        // SAFETY: test-only, unique var.
        unsafe { std::env::set_var(&var, "correct horse battery") };
        let (pp_source, mek_new) = passphrase_source(root, "correct horse battery", &var);

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().unwrap();
        let new_index_cipher = index_cipher_for_source(&pp_source);
        keyring.add_generation(2, Arc::clone(&new_index_cipher));

        // Ledger carries the KCV of the new (passphrase) MEK.
        let mut ledger =
            RotationLedger::index_scope(RotationDirection::Forward, 2, pp_source.clone());
        ledger.mek_kcv = Some(super::compute_mek_kcv(&mek_new).unwrap());
        super::write_ledger(&manager, &ledger).unwrap();

        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring,
            1,
            Arc::clone(
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

        // Resume: re-unwraps the AEKF via the passphrase env, KCV matches, finishes.
        let resume_mgr = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg, None, None);
        // SAFETY: test-only cleanup.
        unsafe { std::env::remove_var(&var) };
        let report = report.unwrap().expect("a pending rotation should resume");
        assert_eq!(report.new_version, 2);
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    #[test]
    fn resume_wrong_passphrase_fails_loud_keeps_ledger() {
        // The env holds the WRONG passphrase → AEKF unwrap fails at load_mek →
        // resume errors loud, ledger retained for a later correct-secret retry.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let var = unique_var("RESUME_WRONGPP");
        // Generate the AEKF (wrapping mek_new) under the RIGHT passphrase, stamp the
        // ledger KCV, then point the env at the WRONG passphrase before resuming.
        let (pp_source, mek_new) = passphrase_source(root, "the-right-one", &var);

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        // Resume fails at load_mek (AEKF unwrap) before any new-generation cipher is
        // used, so no gen-2 seeding is needed. Stamp the ledger, then set the wrong
        // passphrase.
        let mut ledger = RotationLedger::index_scope(RotationDirection::Forward, 2, pp_source);
        ledger.mek_kcv = Some(super::compute_mek_kcv(&mek_new).unwrap());
        super::write_ledger(&manager, &ledger).unwrap();
        // SAFETY: test-only — set the WRONG passphrase for the resume.
        unsafe { std::env::set_var(&var, "the-WRONG-one") };

        let err = resume_pending_rotation(&manager, &enc_cfg, None, None);
        // SAFETY: cleanup.
        unsafe { std::env::remove_var(&var) };
        assert!(err.is_err(), "wrong passphrase must fail loud");
        assert!(
            root.join("data").join("rotation.state").exists(),
            "ledger must be retained after a loud failure"
        );
    }

    #[test]
    fn resume_source_key_changed_kcv_mismatch_precise_error() {
        // The source yields a DIFFERENT but valid key than at rotation start (the
        // AEKF was replaced out-of-band — the Vault-value-changed scenario) → the
        // KCV catches it with a precise, actionable error, ledger retained.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let var = unique_var("RESUME_KCV");
        // SAFETY: test-only.
        unsafe { std::env::set_var(&var, "stable-pass") };
        // MEK-A at rotation start; ledger KCV stamped from MEK-A.
        let (pp_source, mek_a) = passphrase_source(root, "stable-pass", &var);

        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let keyring = manager.keyring().unwrap();
        keyring.add_generation(2, index_cipher_for_source(&pp_source));
        let mut ledger =
            RotationLedger::index_scope(RotationDirection::Forward, 2, pp_source.clone());
        ledger.mek_kcv = Some(super::compute_mek_kcv(&mek_a).unwrap());
        super::write_ledger(&manager, &ledger).unwrap();

        // Out-of-band: replace the AEKF with one wrapping a DIFFERENT MEK-B under
        // the same passphrase.
        let (_pp_source_b, _mek_b) = passphrase_source(root, "stable-pass", &var);

        let err = resume_pending_rotation(&manager, &enc_cfg, None, None);
        // SAFETY: cleanup.
        unsafe { std::env::remove_var(&var) };
        let err = err.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("KCV") && msg.to_lowercase().contains("does not match"),
            "expected a precise KCV-mismatch error, got: {msg}"
        );
        assert!(
            root.join("data").join("rotation.state").exists(),
            "ledger must be retained after a KCV mismatch"
        );
    }

    // ── KMS / Vault loud-and-resumable failure paths (no network needed) ───────

    #[test]
    fn resume_to_kms_unreachable_fails_loud_keeps_ledger() {
        // Resume to a KMS source whose endpoint is unreachable (a closed local
        // port). Whether or not `encryption-aws-kms` is compiled in, the resume
        // must fail LOUD (transport error, or Unavailable "not compiled in") and
        // RETAIN the ledger — never silently drop the rotation.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let kms = KeyProviderConfig::Kms {
            key_id: "alias/test".to_string(),
            encrypted_data_key: "QUJDRA==".to_string(),
            region: Some("us-east-1".to_string()),
            endpoint_url: Some("http://127.0.0.1:1".to_string()),
        };
        super::write_ledger(
            &manager,
            &RotationLedger::index_scope(RotationDirection::Forward, 2, kms),
        )
        .unwrap();
        assert!(resume_pending_rotation(&manager, &enc_cfg, None, None).is_err());
        assert!(
            root.join("data").join("rotation.state").exists(),
            "ledger must be retained so a later reachable-endpoint reopen resumes"
        );
    }

    #[test]
    fn resume_to_vault_token_env_missing_fails_loud_keeps_ledger() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        {
            let db = build_db(root, &old_key);
            seed(&db);
        }
        let var = unique_var("VAULT_MISSING");
        // SAFETY: ensure the token var is absent.
        unsafe { std::env::remove_var(&var) };
        let enc_cfg = EncryptionConfig::file_based(&old_key);
        let manager = Arc::new(IndexPersistenceManager::with_cipher(
            root.join("data"),
            Some(Arc::clone(
                EncryptionManager::from_config(&enc_cfg)
                    .unwrap()
                    .index_cipher(),
            )),
        ));
        let vault = KeyProviderConfig::Vault {
            address: "https://127.0.0.1:1".to_string(),
            token_env: var,
            mount: "secret".to_string(),
            path: "mek".to_string(),
            key_field: "key".to_string(),
            namespace: None,
            ca_cert: None,
        };
        super::write_ledger(
            &manager,
            &RotationLedger::index_scope(RotationDirection::Forward, 2, vault),
        )
        .unwrap();
        assert!(resume_pending_rotation(&manager, &enc_cfg, None, None).is_err());
        assert!(root.join("data").join("rotation.state").exists());
    }

    // ── Guard-lift / forward-rotation to a secret-backed source ───────────────

    #[test]
    fn rotate_index_keys_to_passphrase_source_succeeds() {
        // The whole point of #3620 at the rotation surface: rotating TO a
        // passphrase source is no longer refused fail-closed — it completes and
        // writes a v3 ledger that would resume.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        let indexes_dir = root.join("data").join("indexes");
        let db = build_db_index_only(root, &old_key);
        seed(&db);
        assert!(assert_all_at_version(&indexes_dir, 1) > 0);

        let var = unique_var("ROT_PP");
        // SAFETY: test-only.
        unsafe { std::env::set_var(&var, "rotate-pass") };
        let (pp_source, _mek) = passphrase_source(root, "rotate-pass", &var);
        let report = db.rotate_index_keys(pp_source);
        // SAFETY: cleanup.
        unsafe { std::env::remove_var(&var) };

        let report = report.expect("rotation to a passphrase source must succeed after #3620");
        assert_eq!(report.new_version, 2);
        assert!(report.files_reencrypted > 0);
        assert!(db.index_rotation_status().unwrap().is_fully_rotated());
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    #[test]
    fn rotate_index_keys_to_kms_unreachable_is_loud_not_the_old_refusal() {
        // Guard-lift proof at the forward surface: rotating to a KMS source no
        // longer returns the "not yet supported" refusal on the variant alone; it
        // now attempts the source and fails LOUD (transport / not-compiled), and
        // writes NO ledger (load_mek(new) fails before the ledger write).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        let db = build_db_index_only(root, &old_key);
        seed(&db);
        let kms = KeyProviderConfig::Kms {
            key_id: "alias/test".to_string(),
            encrypted_data_key: "QUJDRA==".to_string(),
            region: Some("us-east-1".to_string()),
            endpoint_url: Some("http://127.0.0.1:1".to_string()),
        };
        let err = db.rotate_index_keys(kms).unwrap_err();
        let msg = format!("{err}");
        assert!(
            !msg.contains("not yet supported"),
            "the old variant-only refusal must be gone, got: {msg}"
        );
        assert!(
            !root.join("data").join("rotation.state").exists(),
            "a failed pre-ledger source load must leave no breadcrumb"
        );
    }

    /// Feature-gate: with `encryption-aws-kms` NOT compiled in, a KMS new source
    /// surfaces `build_provider`'s Unavailable as a clean pre-flight error (no
    /// panic) and writes no ledger.
    #[cfg(not(feature = "encryption-aws-kms"))]
    #[test]
    fn rotate_to_kms_without_feature_refuses_cleanly() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        let db = build_db_index_only(root, &old_key);
        seed(&db);
        let kms = KeyProviderConfig::Kms {
            key_id: "alias/test".to_string(),
            encrypted_data_key: "QUJDRA==".to_string(),
            region: None,
            endpoint_url: None,
        };
        assert!(db.rotate_index_keys(kms).is_err());
        assert!(!root.join("data").join("rotation.state").exists());
    }

    /// Slice 5 (legacy ledger): a v2 ledger written before Slice 5 (no
    /// `layer.subject_keyring=` line) must parse with `subject_keyring == Skipped`
    /// — never fabricate a Pending re-wrap pass — mirroring `wal_retire`.
    #[test]
    fn legacy_v2_ledger_without_subject_keyring_parses_as_skipped() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let path = data_dir.join("rotation.state");
        std::fs::write(
            &path,
            b"version=2\ndirection=forward\ntarget_version=2\nnew_source_kind=file\n\
              new_source_value=/k/new.key\nlayer.index=pending\nlayer.checkpoint=pending\n\
              layer.wal=skipped\nlayer.cold=skipped\nlayer.wal_retire=skipped\n",
        )
        .unwrap();
        let ledger = super::read_rotation_state_at(&path)
            .unwrap()
            .expect("ledger present");
        assert_eq!(
            ledger.subject_keyring,
            super::LayerStatus::Skipped,
            "absent layer.subject_keyring must default to Skipped"
        );
    }

    /// Slice 5 rotation integration: subject-keyring re-wrap on full-MEK rotation.
    /// Gated on `audit-export` (the crypto-shred feature).
    #[cfg(feature = "audit-export")]
    mod subject_keyring_rewrap {
        use super::{build_db, seed};
        use crate::core::property::{PropertyMapBuilder, PropertyValue};
        use crate::db::AletheiaDB;
        use crate::db::crypto_shred::{CryptoShredState, DesignationTarget, SubjectId};
        use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
        use crate::encryption::factory::{Algorithm, create_cipher};
        use std::path::Path;
        use std::sync::atomic::Ordering;
        use tempfile::TempDir;

        const KR_SECRET: &str = "SLICE5_SUBJECT_SECRET_VALUE_dead_beef";

        fn gen_keys(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
            let old_key = root.join("old.key");
            let new_key = root.join("new.key");
            crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
            crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();
            (old_key, new_key)
        }

        /// Create a node, designate `subject` over it, and seal a secret property.
        fn seed_subject(db: &AletheiaDB, subject: &str) -> crate::core::NodeId {
            let id = db
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("stage", "placeholder")
                        .build(),
                )
                .unwrap();
            db.designate_subject(subject, vec![DesignationTarget::WholeNode(id.as_u64())])
                .unwrap();
            db.replace_node(
                id,
                "Person",
                PropertyMapBuilder::new().insert("email", KR_SECRET).build(),
            )
            .unwrap();
            db.persist_indexes().unwrap();
            id
        }

        /// The durable keyring path for a `build_db`-rooted DB (chain_data_dir =
        /// wal-dir parent = `root`).
        fn keyring_path(root: &Path) -> std::path::PathBuf {
            root.join(crate::db::crypto_shred::keyring::KEYRING_FILENAME)
        }

        /// Assert a subject's on-disk wrapped DEK unwraps under a subject-wrap
        /// cipher derived from `key` at `version` — i.e. it is decryptable under
        /// that MEK. Uses a FRESH keyring load (empty DEK cache) so the check
        /// exercises the on-disk wrapped blob, not a cached key.
        fn assert_decryptable_under(root: &Path, subject: &str, key: &Path, alg: Algorithm) {
            let mek = super::super::load_mek(&KeyProviderConfig::File {
                path: key.to_path_buf(),
            })
            .unwrap();
            let dek = super::super::derive_subject_wrap_dek(mek).unwrap();
            let cipher = create_cipher(alg, &dek);
            let fresh = CryptoShredState::open(Some(keyring_path(root))).unwrap();
            fresh
                .subject_key(&SubjectId::new(subject).unwrap(), cipher.as_ref())
                .expect("subject DEK must unwrap under the given key");
        }

        fn plant_keyring_pending_ledger(
            manager: &crate::storage::index_persistence::IndexPersistenceManager,
            new_key: &Path,
        ) {
            super::super::write_ledger(
                manager,
                &super::super::RotationLedger {
                    direction: super::super::RotationDirection::Forward,
                    new_version: 2,
                    new_source: KeyProviderConfig::File {
                        path: new_key.to_path_buf(),
                    },
                    index: super::super::LayerStatus::Skipped,
                    checkpoint: super::super::LayerStatus::Skipped,
                    wal: super::super::LayerStatus::Skipped,
                    cold: super::super::LayerStatus::Skipped,
                    wal_retire: super::super::LayerStatus::Skipped,
                    mek_kcv: None,
                    subject_keyring: super::super::LayerStatus::Pending,
                    algorithm: None,
                },
            )
            .unwrap();
        }

        /// Case 1: a full-MEK rotation re-wraps the subject keyring (key version
        /// advances) and the sealed property still decrypts after a genuine reopen
        /// under the new key.
        #[test]
        fn rotation_rewraps_keyring_value_decrypts_under_new_key() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let node_id = {
                let db = build_db(root, &old_key);
                let id = seed_subject(&db, "subj");
                assert_eq!(
                    db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                    Some(1)
                );
                let report = db
                    .rotate_index_keys(KeyProviderConfig::File {
                        path: new_key.clone(),
                    })
                    .unwrap();
                assert_eq!(report.new_version, 2);
                assert!(!root.join("data").join("rotation.state").exists());
                assert_eq!(
                    db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                    Some(2),
                    "keyring re-wrapped to the new generation"
                );
                id
            };

            let db = build_db(root, &new_key);
            assert!(db.crypto_shred.is_active("subj"));
            let node = db.get_node(node_id).unwrap();
            assert_eq!(
                node.properties.get("email"),
                Some(&PropertyValue::from(KR_SECRET.to_string())),
                "sealed value decrypts under the new MEK after rotation"
            );
        }

        /// Case 2: an erased subject stays erased across a rotation whose re-wrap
        /// pass DOES run (a co-resident active subject makes the layer in scope) —
        /// the wrapped blob never reappears and the attestation survives.
        #[test]
        fn rotation_leaves_erased_subject_erased() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let node_a = {
                let db = build_db(root, &old_key);
                let a = seed_subject(&db, "A");
                let e = seed_subject(&db, "E");
                db.erase_subject("E").unwrap();
                db.persist_indexes().unwrap();
                assert!(db.crypto_shred.is_erased("E"));
                assert!(db.crypto_shred.subject_wrapped_dek_for_test("E").is_none());
                let erased_at = db.crypto_shred.subject_erased_at("E");
                assert!(erased_at.is_some(), "attestation/erased_at recorded");

                // The active subject A makes the keyring layer in scope.
                assert!(db.crypto_shred.keyring_has_stale_wrapped_entries(2));
                let report = db
                    .rotate_index_keys(KeyProviderConfig::File {
                        path: new_key.clone(),
                    })
                    .unwrap();
                assert_eq!(report.new_version, 2);

                assert_eq!(
                    db.crypto_shred.subject_wrapped_key_version_for_test("A"),
                    Some(2),
                    "active subject A re-wrapped"
                );
                assert!(db.crypto_shred.is_erased("E"), "E still erased");
                assert!(
                    db.crypto_shred.subject_wrapped_dek_for_test("E").is_none(),
                    "erased subject's wrapped blob must never reappear"
                );
                assert_eq!(
                    db.crypto_shred.subject_erased_at("E"),
                    erased_at,
                    "erased_at/attestation unchanged across rotation"
                );
                let _ = e;
                a
            };

            let db = build_db(root, &new_key);
            assert!(db.crypto_shred.is_active("A"));
            assert!(db.crypto_shred.is_erased("E"));
            assert_eq!(
                db.get_node(node_a).unwrap().properties.get("email"),
                Some(&PropertyValue::from(KR_SECRET.to_string()))
            );
        }

        /// Case 3: a crash mid-re-wrap resumes via the STARTUP path
        /// (`finalize_resumed_subject_keyring_rotation`, exactly what `db::config`
        /// invokes) under the old key, completing the re-wrap and clearing the
        /// ledger; every entry ends at the new generation and decrypts.
        #[test]
        fn crash_resume_startup_path_completes_rewrap() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let db = build_db(root, &old_key);
            seed_subject(&db, "subj");
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            plant_keyring_pending_ledger(&manager, &new_key);
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(1)
            );

            // Startup resume under the OLD key finishes the deferred re-wrap.
            let enc_old = EncryptionConfig::file_based(&old_key);
            super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_old,
                db.crypto_shred.as_ref(),
            )
            .unwrap();

            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2),
                "re-wrap completed on resume"
            );
            assert!(
                !root.join("data").join("rotation.state").exists(),
                "the keyring settles last and clears the ledger"
            );
            assert_decryptable_under(root, "subj", &new_key, enc_old.algorithm);
        }

        /// Case 4: a crash mid-re-wrap resumes via the LIVE path
        /// (`resume_pending_index_rotation`). The rotation completes index+WAL,
        /// faults before the keyring re-wrap, and the live resume finishes it.
        #[test]
        fn crash_resume_live_path_completes_rewrap() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let db = build_db(root, &old_key);
            let node_id = seed_subject(&db, "subj");

            super::super::FAIL_BEFORE_SUBJECT_KEYRING_REWRAP.store(true, Ordering::SeqCst);
            let err = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .unwrap_err();
            assert!(
                err.to_string().contains("injected"),
                "expected injected fault: {err}"
            );
            assert!(
                root.join("data").join("rotation.state").exists(),
                "the ledger survives the fault with the keyring layer pending"
            );
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(1),
                "keyring not yet re-wrapped"
            );

            db.resume_pending_index_rotation()
                .unwrap()
                .expect("a pending rotation resumes");
            assert!(
                !root.join("data").join("rotation.state").exists(),
                "live resume completes the re-wrap and clears the ledger"
            );
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2)
            );

            drop(db);
            let db = build_db(root, &new_key);
            assert_eq!(
                db.get_node(node_id).unwrap().properties.get("email"),
                Some(&PropertyValue::from(KR_SECRET.to_string())),
                "graph + sealed value read under the new key after resume"
            );
        }

        /// Case 5 (idempotent re-run) — see the focused unit tests in
        /// `crypto_shred::tests` (`rewrap_keyring_is_idempotent_no_op_on_rerun`).
        /// Case 6 (partial-version) — see
        /// `rewrap_keyring_tolerates_mixed_versions_and_advances_only_stale`.
        /// Case 9 (cache invalidation) — see
        /// `rewrap_keyring_invalidates_memoized_wrap_cipher`, plus the cross-reopen
        /// coverage in case 1 above (a reopen under the new key rebuilds the
        /// subject-wrap cipher from scratch and reads sealed values).
        #[test]
        fn crash_resume_idempotent_when_ledger_already_settled() {
            // Case 5 at the rotation seam: re-running the finalize after it already
            // completed is a clean no-op (no ledger -> Ok, nothing re-wrapped).
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);
            let db = build_db(root, &old_key);
            seed_subject(&db, "subj");
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            plant_keyring_pending_ledger(&manager, &new_key);
            let enc_old = EncryptionConfig::file_based(&old_key);
            super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_old,
                db.crypto_shred.as_ref(),
            )
            .unwrap();
            // Second run: ledger already cleared -> guarded no-op.
            super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_old,
                db.crypto_shred.as_ref(),
            )
            .unwrap();
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2)
            );
        }

        /// Case 7: reopening under the NEW key alone mid-pass fails LOUDLY (AEAD
        /// authentication error, never silent wrong data); reopening under the OLD
        /// key finishes the resume.
        #[test]
        fn fail_closed_on_new_key_reopen_then_old_key_resumes() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let db = build_db(root, &old_key);
            seed_subject(&db, "subj");
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            plant_keyring_pending_ledger(&manager, &new_key);

            // Reopen under the NEW key alone -> the old-generation wrapped DEK
            // cannot be unwrapped -> loud error, ledger + keyring untouched.
            let enc_new = EncryptionConfig::file_based(&new_key);
            let err = super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_new,
                db.crypto_shred.as_ref(),
            )
            .unwrap_err();
            // T-ac7: the failure must be a LOUD AEAD authentication failure
            // (wrong key can't unwrap the old-generation DEK), never a silent
            // wrong-data success. The re-wrap surfaces `CryptoShredError::Crypto`
            // ("crypto-shred key operation failed: Decryption failed: ...")
            // through the rotation error wrapper.
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("decryption failed") || msg.contains("crypto-shred key operation"),
                "expected a loud AEAD/crypto failure, got: {err}"
            );
            assert!(
                root.join("data").join("rotation.state").exists(),
                "ledger must stay pending after a failed new-key resume"
            );
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(1),
                "keyring untouched after the loud failure"
            );

            // Reopen under the OLD key -> the resume completes losslessly.
            let enc_old = EncryptionConfig::file_based(&old_key);
            super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_old,
                db.crypto_shred.as_ref(),
            )
            .unwrap();
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2)
            );
            assert!(!root.join("data").join("rotation.state").exists());
        }

        /// Case 8a: a rotation of an encrypted DB with NO designated subjects skips
        /// the keyring layer entirely (no keyring file is ever written).
        #[test]
        fn rotation_skips_keyring_when_no_subjects() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);
            let db = build_db(root, &old_key);
            seed(&db); // normal nodes, no designation
            assert!(!db.crypto_shred.keyring_has_stale_wrapped_entries(2));

            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .unwrap();
            assert_eq!(report.new_version, 2);
            assert!(!root.join("data").join("rotation.state").exists());
            assert!(
                !keyring_path(root).exists(),
                "no subjects -> the keyring layer never writes subject_keyring.dat"
            );
        }

        /// Case 8b: a rotation whose keyring holds ONLY erased subjects skips the
        /// re-wrap (an erased subject has no wrapped DEK -> nothing stale) and the
        /// subject stays erased.
        #[test]
        fn rotation_skips_keyring_when_all_erased() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);
            let db = build_db(root, &old_key);
            seed_subject(&db, "only");
            db.erase_subject("only").unwrap();
            db.persist_indexes().unwrap();
            assert!(
                !db.crypto_shred.keyring_has_stale_wrapped_entries(2),
                "all-erased keyring has nothing stale to re-wrap"
            );

            let report = db
                .rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .unwrap();
            assert_eq!(report.new_version, 2);
            assert!(db.crypto_shred.is_erased("only"));

            drop(db);
            let db = build_db(root, &new_key);
            assert!(db.crypto_shred.is_erased("only"));
        }

        /// Derive the subject-wrap cipher for `key` as an `Arc<dyn Cipher>` — the
        /// shape `rewrap_keyring` takes for its NEW wrapping cipher.
        fn subject_wrap_cipher(
            key: &Path,
            alg: Algorithm,
        ) -> std::sync::Arc<dyn crate::encryption::Cipher> {
            let mek = super::super::load_mek(&KeyProviderConfig::File {
                path: key.to_path_buf(),
            })
            .unwrap();
            let dek = super::super::derive_subject_wrap_dek(mek).unwrap();
            std::sync::Arc::from(create_cipher(alg, &dek))
        }

        /// T-save-then-crash: the re-wrap durably SAVED the keyring at v2 but the
        /// process crashed BEFORE marking the ledger complete. The resumed
        /// finalize must re-run the re-wrap as a 0-entry no-op (skip-if->=version)
        /// under the OLD key — never double-wrapping or corrupting — then clear
        /// the ledger. A fresh `CryptoShredState::open` reload confirms the on-disk
        /// wrapped DEK still unwraps under the NEW key at v2.
        #[test]
        fn crash_after_save_before_mark_resumes_as_zero_op() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            let db = build_db(root, &old_key);
            seed_subject(&db, "subj");
            let manager = db.persistence_manager.as_ref().unwrap().clone();
            let alg = EncryptionConfig::file_based(&old_key).algorithm;

            // "Run one rewrap_keyring" directly: advance + durably persist the
            // keyring to v2, exactly as a completed re-wrap save would leave it
            // just before a crash (index files stay under the old key).
            let old_c = subject_wrap_cipher(&old_key, alg);
            let new_c = subject_wrap_cipher(&new_key, alg);
            let n = db
                .crypto_shred
                .rewrap_keyring(old_c.as_ref(), &new_c, 2)
                .unwrap();
            assert_eq!(n, 1, "pre-advance re-wrapped the single subject to v2");
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2)
            );

            // Plant the Pending ledger the crash left behind (mark/clear never ran).
            plant_keyring_pending_ledger(&manager, &new_key);

            // Resume under the OLD key: every entry is already at v2, so the pass
            // re-wraps ZERO entries (the old cipher is never even consulted) and
            // clears the ledger.
            let enc_old = EncryptionConfig::file_based(&old_key);
            super::super::finalize_resumed_subject_keyring_rotation(
                &manager,
                &enc_old,
                db.crypto_shred.as_ref(),
            )
            .unwrap();
            assert_eq!(
                db.crypto_shred.subject_wrapped_key_version_for_test("subj"),
                Some(2),
                "no double-wrap on the idempotent resume"
            );
            assert!(
                !root.join("data").join("rotation.state").exists(),
                "the settled resume clears the ledger"
            );

            // Fresh reload: the on-disk wrapped DEK is intact and unwraps under
            // the NEW key at v2 (not corrupted / double-wrapped).
            assert_decryptable_under(root, "subj", &new_key, alg);
        }

        /// T-ac9-behavioral: after a LIVE rotation (no reopen), sealing a NEW
        /// property value for an existing subject must seal under the NEW MEK —
        /// proving `rewrap_keyring` refreshed the memoized subject-wrap cipher to
        /// the new cipher (FIX 1), not left the stale old one (which
        /// `subject_wrap_cipher()` would rebuild, since rotation does not swap
        /// `encryption_config`). Reopening under the NEW key alone must read it.
        #[test]
        fn live_rotation_seals_new_value_under_new_mek() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let (old_key, new_key) = gen_keys(root);

            const POST_ROTATION_SECRET: &str = "POST_ROTATION_SECRET_cafef00d";

            let node_id = {
                let db = build_db(root, &old_key);
                let id = seed_subject(&db, "subj");
                db.rotate_index_keys(KeyProviderConfig::File {
                    path: new_key.clone(),
                })
                .unwrap();

                // LIVE (no reopen): seal a NEW value for the existing subject.
                // With FIX 1 the memo now holds the NEW subject-wrap cipher, so
                // this seals under the new MEK.
                db.replace_node(
                    id,
                    "Person",
                    PropertyMapBuilder::new()
                        .insert("email", POST_ROTATION_SECRET)
                        .build(),
                )
                .unwrap();
                db.persist_indexes().unwrap();
                id
            };

            // Reopen under the NEW key ALONE: a value sealed with the stale old
            // cipher would be undecryptable here.
            let db = build_db(root, &new_key);
            assert_eq!(
                db.get_node(node_id).unwrap().properties.get("email"),
                Some(&PropertyValue::from(POST_ROTATION_SECRET.to_string())),
                "a value sealed live after rotation must decrypt under the new MEK"
            );
        }
    }
}
