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
//! * Only the **index** layer is rotated. WAL, cold-storage, and checkpoint
//!   re-encryption are owned by those modules and are tracked as follow-ups.
//! * The rotation is driven synchronously and expects the caller to quiesce
//!   heavy index persistence for its duration; live reads stay correct
//!   throughout (keyring dispatch), and any concurrent index write uses the new
//!   generation (the manager's keyring is already switched).
//! * After a successful rotation the operator MUST point the database's
//!   `key_provider` config at the new key source for subsequent restarts; the
//!   `rotation.state` breadcrumb lets an *interrupted* rotation resume against
//!   the still-current old config.
//! * Audit events are emitted to a locally-constructed key-events logger; the
//!   shared audit sink is wired with Issue #489.

use std::sync::Arc;
use std::time::Instant;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::encryption::audit::{AuditEvent, AuditLevel, EncryptionAuditLogger};
use crate::encryption::cipher::Cipher;
use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
use crate::encryption::factory::create_cipher;
use crate::encryption::key_derivation::KeyDerivation;
use crate::encryption::key_provider::{EnvKeyProvider, FileKeyProvider, KeyProvider};
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
    /// * the new key derives the same index DEK as the current key;
    /// * a key provider cannot source an MEK;
    /// * a filesystem or decryption error occurs during re-encryption.
    pub fn rotate_index_keys(&self, new_key_source: KeyProviderConfig) -> Result<RotationReport> {
        let manager = self.require_rotation_prereqs()?;
        let enc_cfg = self
            .encryption_config
            .clone()
            .ok_or_else(|| rotation_err(RotationError::NotConfigured))?;

        let started = Instant::now();
        let report = self.run_rotation(&manager, &enc_cfg, &new_key_source, started);

        let logger = EncryptionAuditLogger::new(AuditLevel::KeyEvents, "aletheiadb");
        match &report {
            Ok(r) => {
                // NOTE: emitted to a local key-events logger; the shared audit
                // sink is wired with Issue #489. Never logs key material.
                logger.log(&AuditEvent::RotationCompleted {
                    new_version: r.new_version,
                    duration_ms: r.duration_ms,
                });
            }
            Err(e) => {
                logger.log(&AuditEvent::RotationFailed {
                    version: manager.keyring().map(|k| k.current_version()).unwrap_or(0),
                    error: e.to_string(),
                });
            }
        }
        report
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

        // Begin: switch the live keyring to two generations and record the
        // durable breadcrumb, then emit the audit event.
        keyring.add_generation(new_version, Arc::clone(&new_cipher));
        write_rotation_state(manager, new_version, new_key_source)?;
        EncryptionAuditLogger::new(AuditLevel::KeyEvents, "aletheiadb").log(
            &AuditEvent::RotationStarted {
                old_version,
                new_version,
            },
        );

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
// flight and the new key source needed to reconstruct the new cipher on
// resume. No key material is written — only the source *reference* (a file path
// or env var name) and the version numbers.

fn rotation_state_path(manager: &IndexPersistenceManager) -> std::path::PathBuf {
    manager.base_path().join("rotation.state")
}

fn write_rotation_state(
    manager: &IndexPersistenceManager,
    new_version: u32,
    new_source: &KeyProviderConfig,
) -> Result<()> {
    let (kind, value) = match new_source {
        KeyProviderConfig::File { path } => ("file", path.to_string_lossy().into_owned()),
        KeyProviderConfig::Env { variable } => ("env", variable.clone()),
    };
    let body = format!(
        "version=1\nnew_version={new_version}\nnew_source_kind={kind}\nnew_source_value={value}\n"
    );
    std::fs::create_dir_all(manager.base_path())
        .map_err(|e| StorageError::io_error(format!("Failed to create data dir: {e}")))?;
    std::fs::write(rotation_state_path(manager), body)
        .map_err(|e| StorageError::io_error(format!("Failed to write rotation.state: {e}")))?;
    Ok(())
}

fn clear_rotation_state(manager: &IndexPersistenceManager) {
    let _ = std::fs::remove_file(rotation_state_path(manager));
}

/// Parsed rotation breadcrumb.
struct PendingRotation {
    new_version: u32,
    new_source: KeyProviderConfig,
}

fn read_rotation_state(manager: &IndexPersistenceManager) -> Option<PendingRotation> {
    let body = std::fs::read_to_string(rotation_state_path(manager)).ok()?;
    let mut new_version = None;
    let mut kind = None;
    let mut value = None;
    for line in body.lines() {
        let (k, v) = line.split_once('=')?;
        match k {
            "new_version" => new_version = v.parse::<u32>().ok(),
            "new_source_kind" => kind = Some(v.to_string()),
            "new_source_value" => value = Some(v.to_string()),
            _ => {}
        }
    }
    let new_source = match kind.as_deref()? {
        "file" => KeyProviderConfig::File {
            path: value?.into(),
        },
        "env" => KeyProviderConfig::Env { variable: value? },
        _ => return None,
    };
    Some(PendingRotation {
        new_version: new_version?,
        new_source,
    })
}

/// Resume an interrupted index key rotation on startup (Issue #488).
///
/// If a `rotation.state` breadcrumb is present, reconstruct the new generation
/// from the recorded source (the current `enc_cfg` supplies the old key),
/// add it to the manager's keyring, re-run the idempotent re-encryption pass
/// (skipping already-migrated files by header), complete, and clear the
/// breadcrumb. A no-op when no rotation was in flight.
///
/// # Errors
///
/// Returns an error if the recorded new key source cannot be sourced or the
/// re-encryption pass fails.
pub fn resume_pending_rotation(
    manager: &Arc<IndexPersistenceManager>,
    enc_cfg: &EncryptionConfig,
) -> Result<Option<RotationReport>> {
    let Some(pending) = read_rotation_state(manager) else {
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
    let progress = engine.re_encrypt(&mut |_| true).map_err(rotation_err)?;
    engine.complete().map_err(rotation_err)?;
    clear_rotation_state(manager);

    EncryptionAuditLogger::new(AuditLevel::KeyEvents, "aletheiadb").log(
        &AuditEvent::RotationCompleted {
            new_version: pending.new_version,
            duration_ms: started.elapsed().as_millis() as u64,
        },
    );

    Ok(Some(RotationReport {
        old_version,
        new_version: pending.new_version,
        files_total: progress.files_total,
        files_reencrypted: progress.files_reencrypted,
        files_skipped: progress.files_skipped,
        duration_ms: started.elapsed().as_millis() as u64,
    }))
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
    use std::path::Path;
    use tempfile::TempDir;

    /// Build an encrypted, persistent DB rooted at `root`, keyed by `key_file`.
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
        let db = build_db(root, &old_key);
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
        let db = build_db(root, &key);
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
}
