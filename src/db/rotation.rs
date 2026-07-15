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
use crate::encryption::factory::create_cipher;
use crate::encryption::key_derivation::KeyDerivation;
use crate::encryption::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider};
use crate::storage::index_persistence::common::ENC_INDEX_KEY_VERSION_V1;
use crate::storage::index_persistence::{
    IndexKeyRotation, IndexPersistenceManager, RotationError, RotationStatus,
};

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
fn load_mek(cfg: &KeyProviderConfig) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    let provider: Box<dyn KeyProvider> = match cfg {
        KeyProviderConfig::File { path } => Box::new(FileKeyProvider::new(path)),
        KeyProviderConfig::Env { variable } => Box::new(EnvKeyProvider::new(variable)),
    };
    provider
        .get_mek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
}

/// Derive the index DEK for a MEK using the shared HKDF context.
fn derive_index_dek(
    mek: Zeroizing<[u8; 32]>,
) -> std::result::Result<Zeroizing<[u8; 32]>, RotationError> {
    KeyDerivation::new(mek)
        .derive_index_dek()
        .map_err(|e| RotationError::KeyProvider(e.to_string()))
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
        let mut layers = Vec::new();
        // WAL: encrypted under the WAL DEK derived from the same MEK.
        if self.wal.is_encrypted() {
            layers.push("wal");
        }
        // Cold storage: when a tiered/cold store is configured and encryption is
        // enabled, its files are under the cold DEK from the same MEK.
        if self.encryption_manager.is_some() && self.historical.read().has_tiered_storage() {
            layers.push("cold_storage");
        }
        layers
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
        resume_pending_rotation(&manager, &enc_cfg)
    }

    fn run_rotation(
        &self,
        manager: &Arc<IndexPersistenceManager>,
        enc_cfg: &EncryptionConfig,
        new_key_source: &KeyProviderConfig,
        started: Instant,
    ) -> Result<RotationReport> {
        // Shared, mutable keyring handle (mutations are observed by the manager).
        let keyring = manager
            .keyring()
            .cloned()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let old_cipher = keyring
            .current_cipher()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
        let old_version = keyring.current_version();
        let new_version = old_version + 1;

        // Derive both index DEKs and refuse a same-key rotation (constant time).
        let old_dek = derive_index_dek(load_mek(&enc_cfg.key_provider).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        let new_dek = derive_index_dek(load_mek(new_key_source).map_err(rotation_err)?)
            .map_err(rotation_err)?;
        if bool::from(old_dek.as_ref().ct_eq(new_dek.as_ref())) {
            return Err(rotation_err(RotationError::SameKey));
        }

        let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));

        // Begin: record the durable breadcrumb BEFORE switching the live keyring
        // to stamp v2, so no v2-encrypted file can ever be fsynced (via a
        // contract-violating concurrent index persist) before the breadcrumb is
        // on stable storage — a power loss can then never strand a v2 file with a
        // lost breadcrumb (Issue #488 P0.2). `write_rotation_state` fsyncs the
        // file and its parent dir; only once it returns do we flip the keyring.
        write_rotation_state(
            manager,
            new_version,
            new_key_source,
            RotationDirection::Forward,
        )?;
        keyring.add_generation(new_version, Arc::clone(&new_cipher));
        self.emit_rotation_audit(&AuditEvent::RotationStarted {
            old_version,
            new_version,
        });

        let engine = IndexKeyRotation::new(
            manager.indexes_path(),
            keyring.clone(),
            old_version,
            old_cipher,
            new_version,
            new_cipher,
        );

        let progress = engine.re_encrypt(&mut |_| true).map_err(rotation_err)?;
        engine.complete().map_err(rotation_err)?;
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

fn rotation_state_path(manager: &IndexPersistenceManager) -> std::path::PathBuf {
    manager.base_path().join("rotation.state")
}

/// Is a rotation breadcrumb present on disk? A present-but-corrupt breadcrumb is
/// surfaced as an error (fail-closed, Issue #488 P1.1) rather than reported as
/// "present" or "absent".
fn rotation_state_present(manager: &IndexPersistenceManager) -> Result<bool> {
    Ok(read_rotation_state(manager)?.is_some())
}

fn write_rotation_state(
    manager: &IndexPersistenceManager,
    new_version: u32,
    new_source: &KeyProviderConfig,
    direction: RotationDirection,
) -> Result<()> {
    let (kind, value) = match new_source {
        KeyProviderConfig::File { path } => ("file", path.to_string_lossy().into_owned()),
        KeyProviderConfig::Env { variable } => ("env", variable.clone()),
    };
    let body = format!(
        "version=1\ndirection={}\nnew_version={new_version}\nnew_source_kind={kind}\nnew_source_value={value}\n",
        direction.as_str()
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

/// Parsed rotation breadcrumb.
struct PendingRotation {
    direction: RotationDirection,
    new_version: u32,
    new_source: KeyProviderConfig,
}

/// Read and parse the rotation breadcrumb.
///
/// Returns `Ok(None)` when absent (no rotation pending), `Ok(Some(_))` when
/// present and well-formed, and `Err(_)` when PRESENT BUT CORRUPT (Issue #488
/// P1.1). A malformed breadcrumb must never be treated as "no rotation" — that
/// would silently skip resuming a real in-flight rotation and, after a
/// key-provider change, leave v2 files undecryptable. Fail closed instead so
/// startup aborts loudly for manual intervention.
fn read_rotation_state(manager: &IndexPersistenceManager) -> Result<Option<PendingRotation>> {
    let path = rotation_state_path(manager);
    let body = match std::fs::read_to_string(&path) {
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

    let mut direction = RotationDirection::Forward; // default for legacy breadcrumbs
    let mut new_version = None;
    let mut kind = None;
    let mut value = None;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(corrupt("malformed line"));
        };
        match k {
            "direction" => {
                direction = match v {
                    "forward" => RotationDirection::Forward,
                    "cancel" => RotationDirection::Cancel,
                    other => return Err(corrupt(&format!("unknown direction {other:?}"))),
                }
            }
            "new_version" => {
                new_version = Some(v.parse::<u32>().map_err(|_| corrupt("bad new_version"))?)
            }
            "new_source_kind" => kind = Some(v.to_string()),
            "new_source_value" => value = Some(v.to_string()),
            "version" => {}
            _ => {}
        }
    }

    let new_version = new_version.ok_or_else(|| corrupt("missing new_version"))?;
    let value = value.ok_or_else(|| corrupt("missing new_source_value"))?;
    let new_source = match kind.as_deref() {
        Some("file") => KeyProviderConfig::File { path: value.into() },
        Some("env") => KeyProviderConfig::Env { variable: value },
        Some(other) => return Err(corrupt(&format!("unknown new_source_kind {other:?}"))),
        None => return Err(corrupt("missing new_source_kind")),
    };
    Ok(Some(PendingRotation {
        direction,
        new_version,
        new_source,
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
) -> Result<Option<RotationReport>> {
    let Some(pending) = read_rotation_state(manager)? else {
        return Ok(None);
    };
    let Some(keyring) = manager.keyring().cloned() else {
        // Encryption not configured on this startup; leave the breadcrumb.
        return Ok(None);
    };
    let started = Instant::now();
    let old_cipher = keyring
        .current_cipher()
        .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;
    let old_version = keyring.current_version();

    let new_dek = derive_index_dek(load_mek(&pending.new_source).map_err(rotation_err)?)
        .map_err(rotation_err)?;
    let new_cipher: Arc<dyn Cipher> = Arc::from(create_cipher(enc_cfg.algorithm, &new_dek));

    keyring.add_generation(pending.new_version, Arc::clone(&new_cipher));

    let engine = IndexKeyRotation::new(
        manager.indexes_path(),
        keyring,
        old_version,
        old_cipher,
        pending.new_version,
        new_cipher,
    );

    let logger = EncryptionAuditLogger::from_audit_config(&enc_cfg.audit);
    let report = match pending.direction {
        RotationDirection::Forward => {
            let progress = engine.re_encrypt(&mut |_| true).map_err(rotation_err)?;
            engine.complete().map_err(rotation_err)?;
            clear_rotation_state(manager);
            logger.log(&AuditEvent::RotationCompleted {
                new_version: pending.new_version,
                duration_ms: started.elapsed().as_millis() as u64,
            });
            RotationReport {
                old_version,
                new_version: pending.new_version,
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
                old_version: pending.new_version,
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
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg)
            .unwrap()
            .expect("a pending rotation should resume");
        assert_eq!(report.new_version, 2);
        assert!(!root.join("data").join("rotation.state").exists());
        assert!(assert_all_at_version(&indexes_dir, 2) > 0);
    }

    // ── P0.1: cross-layer MEK-desync guard ───────────────────────────

    #[test]
    fn rotate_refuses_when_wal_encrypted() {
        // A fully-encrypted DB (WAL under the same MEK) must REFUSE an index-only
        // rotation: rotating the index alone to a new MEK then switching the key
        // provider would strand the WAL under the old key (catastrophic loss).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let old_key = root.join("old.key");
        let new_key = root.join("new.key");
        crate::encryption::FileKeyProvider::generate_key_file(&old_key).unwrap();
        crate::encryption::FileKeyProvider::generate_key_file(&new_key).unwrap();

        let db = build_db(root, &old_key); // WAL encrypted too
        seed(&db);
        assert!(db.wal.is_encrypted(), "precondition: WAL is encrypted");

        let err = db
            .rotate_index_keys(KeyProviderConfig::File {
                path: new_key.clone(),
            })
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("other encrypted-at-rest layers") && msg.contains("wal"),
            "expected cross-layer refusal naming the WAL, got: {msg}"
        );
        // Refusal leaves NO breadcrumb behind (checked before any state write).
        assert!(!root.join("data").join("rotation.state").exists());
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
        let err = resume_pending_rotation(&manager, &enc_cfg).unwrap_err();
        assert!(
            err.to_string().contains("corrupt"),
            "corrupt breadcrumb must abort startup, got: {err}"
        );
        // And an ABSENT breadcrumb is a clean no-op (distinguish absent vs corrupt).
        std::fs::remove_file(data_dir.join("rotation.state")).unwrap();
        assert!(
            resume_pending_rotation(&manager, &enc_cfg)
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
        let report = resume_pending_rotation(&resume_mgr, &enc_cfg)
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
}
