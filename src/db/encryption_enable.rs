//! Plaintext → encrypted-at-rest migration engine (Issue #3616 PR3).
//!
//! This is the engine behind `aletheia encryption enable`: it takes a database
//! that was created **plaintext** and migrates the WAL through the cipher, then
//! flips the durable [`encryption.state`](crate::db::encryption_state) authority
//! so the *next* [`open()`](crate::AletheiaDB::open) comes up encrypted.
//!
//! It composes the two foundations that already landed:
//! * **PR1 (#3657)** — the durable [`encryption.state`] authority the migration
//!   flips (authority WINS over the operator's TOML/env config).
//! * **PR2 (#3669)** — the WAL runtime
//!   [`install_wal_keyring`](crate::storage::wal::concurrent_system::ConcurrentWalSystem::install_wal_keyring)
//!   seam that flips a live plaintext WAL to encrypted (`None → Some`) crash-consistently.
//!
//! # Migration ordering (the crux — do not reorder)
//!
//! 1. **Quiesce the background index-persistence thread FIRST** (stop + join).
//!    While a live keyring is still `None`, a background persist could overwrite
//!    a freshly-encrypted file with plaintext. The stop happens while the whole
//!    database is still plaintext, so the worker's shutdown final-persist is a
//!    safe plaintext write.
//! 2. **Migrate the WAL** — install the DEK keyring via the PR2 seam (which seals
//!    the plaintext segment and force-rolls to a fresh encrypted v16 segment),
//!    recording progress as a per-field [`LayerStatus`](crate::db::rotation) in a
//!    durable `direction=enable` rotation ledger so a crash mid-migration resumes
//!    idempotently.
//! 3. **Flip the [`encryption.state`] authority to `enabled` BEFORE clearing the
//!    ledger.** This binding order closes the crash gap: were the ledger cleared
//!    first and a crash struck before the authority flip, the next `open()` would
//!    read the unchanged plaintext config over now-encrypted WAL bytes and
//!    mis-read (undecodable) ciphertext as plaintext. With the authority flipped
//!    first, a crash between the two steps leaves a still-present ledger that
//!    [`resume_pending_enable`] reconciles on the next `open()`.
//! 4. **Clear the ledger.**
//!
//! # In scope vs deferred (WAL-first slice)
//!
//! In scope for this slice: **WAL**. Index / checkpoint / cold are recorded
//! [`LayerStatus::Skipped`](crate::db::rotation) and are **not** wrapped yet:
//! * **Index / checkpoint** files remain plaintext at rest; this is *safe* on
//!   reopen because the encrypted index reader header-sniffs and reads plaintext
//!   files transparently. A follow-up worker adds the plaintext→`AEIX` wrap pass
//!   (flipping these layers to `Pending`).
//! * **Cold** storage is *refused* up front when present (a `NotImplemented`
//!   error): unlike the index reader, the cold reader treats a bare value as
//!   legacy ciphertext and would fail AEAD auth on reopen, so leaving cold bare
//!   under an `enabled` authority is unsafe. The bare→`ACV1` wrap pass is a
//!   follow-up.
//!
//! Also deferred (NOT migrated by v1): `.albk` backups, `snapshots.json`, auth
//! `keys.json`, `schema_constraints.dat`.
//!
//! # Crash resume
//!
//! An interrupted enable has NOT yet flipped the authority, so
//! `config.encryption.enabled` is still `false` — which means the rotation resume
//! paths ([`resume_pending_rotation`](crate::db::rotation), which gate on
//! `enabled == true`) will NOT fire, and they additionally skip any
//! `direction=enable` ledger outright. That is why this module ships its own
//! [`resume_pending_enable`] reconciler (plus the pre-replay
//! [`install_pending_enable_wal_keyring`](crate::db::rotation::install_pending_enable_wal_keyring)
//! hook that lets the startup replay read decrypt an already-rolled WAL), both
//! wired into `open()`.

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::db::encryption_state::{
    EncryptionState, read_encryption_state, write_encryption_state_durable,
};
use crate::db::rotation::{
    build_enable_wal_keyring, clear_rotation_state, mark_wal_complete, read_enable_ledger,
    write_enable_ledger,
};
use crate::encryption::config::KeyProviderConfig;
use crate::encryption::factory::Algorithm;

/// Outcome of a completed plaintext → encrypted enable migration.
///
/// Each flag reports whether that at-rest layer actually had bytes migrated
/// through the cipher. In the WAL-first slice only `wal_migrated` is ever `true`;
/// index / checkpoint / cold are `false` (deferred — see the module docs). The
/// key-source reference recorded into the durable
/// [`encryption.state`](crate::db::encryption_state) authority is echoed so the
/// operator can confirm what the next `open()` will consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableReport {
    /// WAL segments rolled to the encrypted (v16) format.
    pub wal_migrated: bool,
    /// Plaintext index files wrapped into the `AEIX` format (deferred → `false`).
    pub index_migrated: bool,
    /// Checkpoint files covered (deferred → `false`).
    pub checkpoint_migrated: bool,
    /// Cold-storage bare values wrapped into the `ACV1` format (deferred → `false`).
    pub cold_migrated: bool,
    /// The non-secret key-source reference recorded into the authority
    /// (File path / Env var name — never key bytes).
    pub key_source: KeyProviderConfig,
}

/// Map the WAL seam's double-install rejection (a `WalError`, which would default
/// to an `INTERNAL`-class error) to a `FailedPrecondition` — enabling an
/// already-encrypted WAL is a caller precondition failure, not an internal fault
/// (honors the PR2 `install_wal_keyring` TODO).
fn map_wal_install_err(e: Error) -> Error {
    match &e {
        Error::Storage(StorageError::WalError { reason }) => Error::FailedPrecondition(format!(
            "cannot enable encryption: the WAL is already encrypted ({reason})"
        )),
        _ => e,
    }
}

impl AletheiaDB {
    /// The AEAD algorithm an enable migration writes under. A plaintext database
    /// has no `encryption_config`, so this falls back to the crate default — which
    /// is exactly what the next `open()` (reading the flipped authority) will also
    /// use, keeping the WAL DEK cipher in lockstep across the reopen.
    fn enable_algorithm(&self) -> Algorithm {
        self.encryption_config
            .as_ref()
            .map(|c| c.algorithm)
            .unwrap_or_default()
    }

    /// Migrate this **plaintext** database to encrypted-at-rest under
    /// `key_source`, in place, crash-consistently (Issue #3616 PR3).
    ///
    /// # ⚠️ LOUD REOPEN CONTRACT — read before calling
    ///
    /// This is a **reopen-centric** migration. When this call returns `Ok`, the
    /// handle you are holding is in a deliberately **partial**, **quiesced**
    /// runtime state:
    ///
    /// * **WAL** is encrypted **live** — the DEK keyring has been installed and
    ///   every subsequent WAL append is written encrypted.
    /// * **Index / checkpoint / cold persistence is QUIESCED** — the background
    ///   index-persistence thread was stopped (and joined) as step 1 and is
    ///   **not restarted by this call**. This is a structural necessity, not a
    ///   shortcut: the [`IndexPersistenceManager`](crate::storage::index_persistence::IndexPersistenceManager)'s
    ///   index keyring is owned **by value** (there is no live `None → Some`
    ///   install as there is for the WAL's atomic-swap cell), so a handle created
    ///   plaintext can never *begin* writing encrypted index files. Restarting a
    ///   persisting worker in-process would therefore write **plaintext** index
    ///   files over a database whose authority now says "encrypted" — the exact
    ///   corruption this engine exists to prevent. The worker is thus respawned
    ///   only by the mandatory **reopen** (a fresh manager built with the index
    ///   keyring), which is where "stop → migrate → respawn" completes.
    ///
    /// ## The quiesced window
    ///
    /// Between this call returning and the reopen, the handle does not perform
    /// periodic index/checkpoint/cold persistence. Continuing to WRITE through the
    /// returned handle is **not supported** in v1 (index mutations would not be
    /// persisted by this handle; they survive only via the encrypted WAL until
    /// reopen). **You MUST reopen the database** (drop this handle and call
    /// [`AletheiaDB::open`]) to get a fully-operational encrypted instance: the
    /// next `open()` reads the flipped [`encryption.state`](crate::db::encryption_state)
    /// authority, brings every layer up under the cipher, and restarts the
    /// background persistence thread.
    ///
    /// # Errors
    ///
    /// Returns a structured error (never a partial silent success) when:
    /// * index persistence is not enabled (an ephemeral / in-memory database
    ///   cannot be migrated — `FailedPrecondition`);
    /// * the database is already encrypted (`FailedPrecondition` — use key
    ///   rotation, not enable);
    /// * `key_source` is a Passphrase/KMS/Vault backend (the authority + ledger
    ///   can only persist a File/Env reference — `FailedPrecondition`);
    /// * a cold-storage tier is present (`NotImplemented` — the bare→`ACV1` wrap
    ///   pass is a follow-up; see the module docs for why it is refused rather
    ///   than silently left bare);
    /// * any layer migration or the durable authority flip fails.
    ///
    /// On any error the binding order guarantees the database is left either fully
    /// plaintext (authority never flipped) or resumable via
    /// [`resume_pending_enable`] on the next `open()` — never a silently
    /// half-encrypted state that reads as plaintext.
    pub fn enable_encryption(&mut self, key_source: KeyProviderConfig) -> Result<EnableReport> {
        // Drive the whole migration off per-field `LayerStatus` checks in the
        // rotation ledger — NEVER off a positional index or a count of layers.

        // === Step 0: preconditions (no side effects — run BEFORE quiesce) ============
        let manager = self.persistence_manager.clone().ok_or_else(|| {
            Error::FailedPrecondition(
                "cannot enable encryption on an ephemeral (in-memory) database: no durable \
                 at-rest bytes or authority file to migrate. Open a durable database first."
                    .to_string(),
            )
        })?;

        if self.encryption_manager.is_some() || self.wal.is_encrypted() {
            return Err(Error::FailedPrecondition(
                "database is already encrypted; use key rotation, not enable".to_string(),
            ));
        }
        if let Some(state) = read_encryption_state(manager.base_path())?
            && state.enabled
        {
            return Err(Error::FailedPrecondition(
                "the durable encryption authority already records this database as encrypted"
                    .to_string(),
            ));
        }

        // Only File/Env sources round-trip through the ledger + authority.
        match &key_source {
            KeyProviderConfig::File { .. } | KeyProviderConfig::Env { .. } => {}
            other => {
                let (provider_type, _) = other.describe();
                return Err(Error::FailedPrecondition(format!(
                    "enabling encryption with a {provider_type} key source is not supported \
                     (only file/env references can be persisted without leaking a secret)"
                )));
            }
        }

        // Cold storage present → refuse (bare values are unsafe under an enabled
        // authority; the wrap pass is a follow-up).
        if self.historical.read().has_tiered_storage() {
            return Err(Error::NotImplemented {
                feature: "enable encryption over an existing cold-storage tier".to_string(),
                reason: "the cold bare->ACV1 wrap pass is a #3616 follow-up; leaving cold values \
                         bare under an enabled authority would fail AEAD auth on reopen"
                    .to_string(),
            });
        }

        // Build the WAL keyring EARLY so an unreadable/short key fails fast, before
        // the background thread is stopped (no side effects if this errors).
        let algorithm = self.enable_algorithm();
        let keyring = build_enable_wal_keyring(&key_source, algorithm)?;

        // === Step 1: quiesce the background index-persistence thread =================
        // Stop + join. The worker's shutdown final-persist runs while the whole DB
        // is still plaintext, so it is a safe plaintext write. NOT restarted here
        // (see the reopen contract) — respawn is the reopen's fresh worker.
        if let Some(tracker) = self.persistence_tracker.as_ref() {
            tracker.signal_shutdown();
        }
        if let Some(handle) = self.persistence_thread_handle.take() {
            let _ = handle.join();
        }

        // === Step 2: durable ledger (breadcrumb #1) then migrate WAL =================
        // Index / checkpoint / cold are Skipped in this slice (index left plaintext
        // — reader-tolerant; cold refused above). WAL is Pending.
        write_enable_ledger(&manager, &key_source, false, false)?;

        // WAL: install the keyring (seal plaintext v13 → store → reopen encrypted
        // v16) via the PR2 seam, then record completion (breadcrumb #2).
        self.wal
            .install_wal_keyring(keyring)
            .map_err(map_wal_install_err)?;
        mark_wal_complete(&manager)?;

        // === Step 3: flip the authority BEFORE clearing the ledger (binding order) ===
        // (breadcrumb #3) A crash after this but before the clear resumes via
        // `resume_pending_enable`.
        write_encryption_state_durable(
            manager.base_path(),
            &EncryptionState::enabled(key_source.clone()),
        )?;

        // === Step 4: clear the ledger (breadcrumb #4) ================================
        clear_rotation_state(&manager);

        Ok(EnableReport {
            wal_migrated: true,
            index_migrated: false,
            checkpoint_migrated: false,
            cold_migrated: false,
            key_source,
        })
    }

    /// Resume an interrupted plaintext → encrypted enable migration at `open()`
    /// time, if one is pending (Issue #3616 PR3).
    ///
    /// Wired into [`with_unified_config`](crate::AletheiaDB::with_unified_config)
    /// alongside the rotation resume paths, but distinct from them: an interrupted
    /// enable has NOT yet flipped the [`encryption.state`](crate::db::encryption_state)
    /// authority, so `config.encryption.enabled` is still `false` and
    /// [`resume_pending_rotation`](crate::db::rotation) never fires (and skips a
    /// `direction=enable` ledger anyway). This reconciler recognizes the
    /// enable-scope ledger and completes the migration idempotently, driving off
    /// each layer's per-field `LayerStatus`.
    ///
    /// By the time this runs at startup the pre-replay
    /// [`install_pending_enable_wal_keyring`](crate::db::rotation::install_pending_enable_wal_keyring)
    /// hook has already installed the WAL keyring when the on-disk WAL was
    /// encrypted, so the replay read decrypted correctly; here we finish any
    /// still-`Pending` WAL work, then flip the authority and clear the ledger.
    ///
    /// **Guarded no-op:** returns `Ok(())` immediately when no durable
    /// pending-enable ledger exists (the overwhelmingly common startup path), so
    /// wiring it unconditionally into `open()` costs at most one ledger probe.
    ///
    /// # Errors
    ///
    /// Propagates a fail-closed error if a present ledger is corrupt, a WAL
    /// keyring cannot be built/installed, or a not-yet-implemented layer
    /// (index/checkpoint/cold) is `Pending` — a corrupt/undecryptable state must
    /// abort startup loudly rather than be treated as "no migration pending".
    pub(crate) fn resume_pending_enable(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_enable_ledger(manager)? else {
            return Ok(());
        };

        // WAL: ensure the keyring is installed + rolled, then record completion.
        // At startup the pre-read hook installs it when the WAL is already
        // encrypted on disk; if it is still plaintext (crash before the roll),
        // install now to seal -> roll -> encrypt.
        if view.wal_pending {
            if !self.wal.is_encrypted() {
                let keyring = build_enable_wal_keyring(&view.new_source, self.enable_algorithm())?;
                self.wal
                    .install_wal_keyring(keyring)
                    .map_err(map_wal_install_err)?;
            }
            mark_wal_complete(manager)?;
        }

        // Index / checkpoint / cold are Skipped in this engine version. A follow-up
        // worker that flips them to Pending MUST run their bare->encrypted passes
        // here (driven off view.index_pending / view.checkpoint_pending /
        // view.cold_pending) BEFORE the authority flip below. Until then a Pending
        // non-WAL layer means a ledger this build cannot complete — fail closed
        // rather than clear it and strand un-wrapped bytes under an enabled
        // authority.
        if !view.non_wal_layers_settled() {
            return Err(Error::NotImplemented {
                feature: "resume of a pending index/checkpoint/cold enable migration".to_string(),
                reason: "the bare->encrypted wrap passes are a #3616 follow-up".to_string(),
            });
        }

        // Binding order: flip the authority BEFORE clearing the ledger (idempotent
        // if already flipped — the flip->clear-gap resume case).
        write_encryption_state_durable(
            manager.base_path(),
            &EncryptionState::enabled(view.new_source.clone()),
        )?;
        clear_rotation_state(manager);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::db::encryption_state::{encryption_state_path, read_encryption_state};
    use crate::db::rotation::write_enable_ledger;
    use crate::encryption::config::KeyProviderConfig;
    use crate::encryption::key_provider::FileKeyProvider;
    use crate::storage::index_persistence::{IndexPersistenceManager, PersistenceConfig};
    use crate::storage::wal::DurabilityMode;
    use crate::{AletheiaDB, PropertyMapBuilder};
    use std::path::Path;
    use std::sync::Arc;

    fn plaintext_durable_config(data_dir: &Path) -> AletheiaDBConfig {
        AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(data_dir.join("wal"))
                    .durability_mode(DurabilityMode::GroupCommit {
                        max_delay_ms: 10,
                        max_batch_size: 200,
                    })
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.join("indexes"),
                load_on_startup: true,
                ..Default::default()
            })
            .build()
    }

    fn key_source_at(dir: &Path) -> KeyProviderConfig {
        let key_path = dir.join("mek.key");
        FileKeyProvider::generate_key_file(&key_path).expect("generate key file");
        KeyProviderConfig::File { path: key_path }
    }

    /// A manager rooted at the same `indexes` dir the durable config uses (its
    /// `persistence.data_dir` == the manager `base_path`).
    fn manager_for(data_dir: &Path) -> Arc<IndexPersistenceManager> {
        Arc::new(IndexPersistenceManager::new(data_dir.join("indexes")))
    }

    fn node_count_after_reopen(data_dir: &Path) -> usize {
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(data_dir)).unwrap();
        db.node_count()
    }

    /// Happy path: enable encrypts the WAL, flips the authority, and clears the
    /// ledger; the reopened database reads back its data and the WAL is encrypted.
    #[test]
    fn enable_encryption_encrypts_wal_flips_authority_clears_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
                .unwrap();
            assert!(!db.wal.is_encrypted(), "starts plaintext");

            let report = db.enable_encryption(key.clone()).unwrap();
            assert!(report.wal_migrated);
            assert!(!report.index_migrated && !report.cold_migrated);
            assert!(db.wal.is_encrypted(), "WAL live-encrypted after enable");

            // Authority flipped on disk.
            let state = read_encryption_state(&indexes).unwrap().unwrap();
            assert!(state.enabled);
            assert_eq!(state.key_source, Some(key.clone()));
            // Ledger cleared.
            assert!(!indexes.join("rotation.state").exists());
        }

        // Reopen under the flipped authority: data survives, WAL is encrypted.
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        assert_eq!(db.node_count(), 1, "node survives reopen via encrypted WAL");
        assert!(db.wal.is_encrypted(), "reopen honors the durable authority");
    }

    /// Enabling an ephemeral (in-memory) database is refused.
    #[test]
    fn enable_encryption_refuses_ephemeral() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let mut db = AletheiaDB::new().unwrap();
        let err = db.enable_encryption(key).unwrap_err();
        assert!(matches!(
            err,
            crate::core::error::Error::FailedPrecondition(_)
        ));
    }

    /// Enabling twice is refused (already-encrypted precondition).
    #[test]
    fn enable_encryption_refuses_when_already_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let mut db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        db.enable_encryption(key.clone()).unwrap();
        let err = db.enable_encryption(key).unwrap_err();
        assert!(matches!(
            err,
            crate::core::error::Error::FailedPrecondition(_)
        ));
    }

    /// A KMS/Vault/passphrase key source is refused up front.
    #[test]
    fn enable_encryption_refuses_secret_backed_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        let err = db
            .enable_encryption(KeyProviderConfig::PassphraseFile {
                path: "/keys/mek.aekf".into(),
                passphrase_env: "MEK_PASS".to_string(),
            })
            .unwrap_err();
        assert!(matches!(
            err,
            crate::core::error::Error::FailedPrecondition(_)
        ));
    }

    /// C0: a crash BEFORE the ledger is written (no ledger, no authority) reopens
    /// as a plain plaintext database — resume is a true no-op.
    #[test]
    fn enable_crash_before_ledger_reopens_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
        }
        // No ledger, no authority → plaintext reopen, data intact.
        assert!(
            read_encryption_state(&dir.path().join("indexes"))
                .unwrap()
                .is_none()
        );
        assert_eq!(node_count_after_reopen(dir.path()), 1);
    }

    /// C1: crash AFTER the ledger (wal=Pending) but BEFORE the WAL roll — the WAL
    /// is still plaintext. Reopen must resume: install the keyring, roll, flip the
    /// authority, clear the ledger.
    #[test]
    fn enable_crash_after_ledger_before_wal_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
        }
        // Simulate the interrupted state: an enable ledger present, WAL untouched.
        write_enable_ledger(&manager_for(dir.path()), &key, false, false).unwrap();
        assert!(indexes.join("rotation.state").exists());

        // Reopen: resume completes the enable.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1);
            assert!(db.wal.is_encrypted(), "resume rolled the WAL to encrypted");
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled);
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C3 (the crux): crash AFTER the WAL roll (wal=Complete, WAL encrypted on
    /// disk) but BEFORE the authority flip. The startup replay read would fail
    /// without a keyring — the pre-read hook must install it from the ledger, then
    /// resume flips the authority and clears the ledger. Encrypted bytes are never
    /// mis-read as plaintext.
    #[test]
    fn enable_crash_after_wal_before_authority_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
            // Full enable: WAL now encrypted on disk, authority flipped, ledger cleared.
            db.enable_encryption(key.clone()).unwrap();
        }
        // Rewind to "after WAL roll, before authority flip": remove the authority
        // and re-lay a wal=Complete enable ledger.
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        assert!(read_encryption_state(&indexes).unwrap().is_none());
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, false, false).unwrap();
            // Force wal=Complete to mirror the real crash point.
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
        }

        // Reopen: config sees no authority (plaintext), but the encrypted WAL must
        // still decrypt via the pre-read hook, and resume must flip + clear.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1, "encrypted WAL decrypted, not mis-read");
            assert!(db.wal.is_encrypted());
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped by resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C4: crash in the flip→clear gap (authority already `enabled`, ledger still
    /// present). Reopen: the rotation resume paths skip the enable ledger; only
    /// `resume_pending_enable` clears it.
    #[test]
    fn enable_crash_after_authority_before_clear_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
            db.enable_encryption(key.clone()).unwrap();
        }
        // Re-lay a settled (wal=Complete) ledger, leaving the authority ENABLED.
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, false, false).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
        }
        assert!(indexes.join("rotation.state").exists());
        assert!(read_encryption_state(&indexes).unwrap().unwrap().enabled);

        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1);
        }
        assert!(
            !indexes.join("rotation.state").exists(),
            "flip->clear gap resume cleared the ledger"
        );
        assert!(read_encryption_state(&indexes).unwrap().unwrap().enabled);
    }

    /// C6: a crash between migrate and respawn is, on disk, a completed enable
    /// (authority flipped, ledger cleared, WAL encrypted). The reopen respawns the
    /// background worker and the database is fully operational.
    #[test]
    fn enable_crash_between_migrate_and_respawn_reopens_clean() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
            db.enable_encryption(key).unwrap();
            // Drop WITHOUT respawning (the handle never restarted its worker).
        }
        // Reopen = respawn: worker back, writes work, data intact.
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        assert_eq!(db.node_count(), 1);
        assert!(db.wal.is_encrypted());
        db.create_node("M", PropertyMapBuilder::new().build())
            .unwrap();
        assert_eq!(db.node_count(), 2);
    }

    /// Cold bare→ACV1 round-trip: DEFERRED. Marked `#[ignore]` as a RED spec
    /// marker for the next worker (the cold wrap pass). Do NOT delete.
    #[test]
    #[ignore = "TODO(#3616 follow-up): cold bare->ACV1 wrap pass not implemented; \
                enable currently refuses when a cold store is present"]
    fn enable_cold_bare_plaintext_roundtrip() {
        // When the cold wrap pass lands: build a durable DB with a cold tier + bare
        // values, enable, reopen, and assert every cold value round-trips as ACV1
        // under the cold DEK (never mis-read as bare ciphertext / never AEAD-fails).
        unimplemented!("cold bare->ACV1 wrap pass — #3616 follow-up");
    }

    /// Index plaintext→AEIX round-trip: DEFERRED. Marked `#[ignore]` as a RED spec
    /// marker for the next worker (the index wrap pass). Do NOT delete.
    #[test]
    #[ignore = "TODO(#3616 follow-up): index plaintext->AEIX wrap pass not implemented; \
                index files are left plaintext (reader-tolerant) by the WAL-first slice"]
    fn enable_index_plaintext_roundtrip() {
        // When the index wrap pass lands: persist plaintext index files, enable,
        // reopen, and assert every index file now carries the AEIX header.
        unimplemented!("index plaintext->AEIX wrap pass — #3616 follow-up");
    }
}
