//! Encrypted → plaintext migration engine (Issue #3616 PR4).
//!
//! This is the engine behind `aletheia encryption disable`: the exact inverse of
//! [`enable_encryption`](crate::db::encryption_enable) — it takes a database that
//! is encrypted-at-rest and strips the cipher from every at-rest layer, then flips
//! the durable [`encryption.state`](crate::db::encryption_state) authority to
//! `disabled` so the *next* [`open()`](crate::AletheiaDB::open) comes up plaintext.
//!
//! It composes the seams that already landed:
//! * **PR1 (#3657)** — the durable [`encryption.state`] authority (the migration
//!   flips it to `disabled`).
//! * **slice 2 (#3616 PR4)** — the WAL runtime
//!   [`uninstall_wal_keyring`](crate::storage::wal::concurrent_system::ConcurrentWalSystem::uninstall_wal_keyring)
//!   seam that strips a live encrypted WAL to plaintext (`Some → None`).
//! * **slice 3 (#3616 PR4)** — the
//!   [`unwrap_encrypted_index_dir`](crate::storage::index_persistence::reencrypt::unwrap_encrypted_index_dir)
//!   and
//!   [`unwrap_encrypted_cold_values`](crate::storage::redb_cold_storage::RedbColdStorage::unwrap_encrypted_cold_values)
//!   passes that rewrite `AEIX` index files / `ACV1` cold values back to plaintext.
//!
//! # Migration ordering (the crux — the crash-safe inverse of enable)
//!
//! 1. **Quiesce the background index-persistence thread FIRST** (stop + join),
//!    while the whole database is still encrypted, so its shutdown final-persist
//!    is a safe *encrypted* write into the durable `AEIX` snapshot.
//! 2. **Synchronous full persist while STILL ENCRYPTED** — capture all current
//!    state into the durable (`AEIX`) index snapshot NOW, so retiring the
//!    encrypted WAL segments below loses nothing (reopen reconstructs from the
//!    snapshot, which the index-unwrap pass turns into plaintext).
//! 3. **Migrate every in-scope at-rest layer**, recording progress as a per-field
//!    [`LayerStatus`](crate::db::rotation) in a durable `direction=disable`
//!    rotation ledger so a crash mid-migration resumes idempotently:
//!    * **WAL** — [`uninstall_wal_keyring`] (seals the encrypted segment, stores
//!      `None`, reopens a fresh plaintext segment), then retire the now-undecryptable
//!      encrypted segments.
//!    * **Index + checkpoint** — an `AEIX` → plaintext unwrap pass rewrites every
//!      wrapped index file back to its bare body under the index DEK.
//!    * **Cold** — an `ACV1` → bare unwrap pass rewrites every wrapped cold value
//!      back to plaintext (only when a cold tier is present).
//! 4. **Flip the [`encryption.state`] authority to `disabled` BEFORE clearing the
//!    ledger.** This binding order is the inverse of enable: the authority MUST
//!    stay `enabled` until every encrypted byte is unwrapped, so a mid-disable
//!    crash reopens under the authority still reading `enabled` and can DECRYPT the
//!    remaining ciphertext to finish the migration. The flip to `disabled` is the
//!    commit point; a crash between the flip and the clear leaves a still-present
//!    ledger that [`resume_pending_disable`](AletheiaDB::resume_pending_disable)
//!    clears on the next `open()`.
//! 5. **Clear the ledger.**
//!
//! # Crash resume
//!
//! An interrupted disable has NOT yet flipped the authority, so it still records
//! `enabled` and `open()` would normally bring every layer up ENCRYPTED. But the
//! disable end state is PLAINTEXT, and a live encrypted index manager / cold store
//! would re-encrypt over the freshly-unwrapped bytes on the next background
//! persist. So `db::config`, on detecting a pending disable ledger, builds every
//! at-rest layer PLAINTEXT (the end state) and threads the recorded key source
//! into the transient resume read passes: the pre-replay
//! [`install_pending_disable_wal_keyring`](crate::db::rotation::install_pending_disable_wal_keyring)
//! hook installs the decrypt keyring so the startup replay decrypts the
//! still-encrypted WAL segments, and [`resume_pending_disable`] finishes the WAL
//! uninstall + retire and the index/cold unwraps before flipping the authority.
//!
//! WAL and index resumes yield a fully-usable session. The COLD layer has one
//! sharp edge: the cold store is built with the decrypt keyring so the resume can
//! unwrap its `ACV1` values, but the cold read path decrypts by keyring presence,
//! so bare (just-unwrapped) values are unreadable through that keyring'd store —
//! the disable durably completes (values bare, authority `disabled`, ledger
//! cleared) but a crashed disable WITH a cold tier wants **one more reopen** for
//! cold reads to work (the next `open()` builds a plaintext cold store). See
//! [`resume_pending_disable_cold`](AletheiaDB::resume_pending_disable_cold).

use crate::core::error::{Error, Result, StorageError};
use crate::db::AletheiaDB;
use crate::db::encryption_state::{EncryptionState, write_encryption_state_durable};
use crate::db::rotation::{
    clear_rotation_state, mark_cold_complete, mark_index_complete, mark_wal_complete,
    mark_wal_retire_complete, read_disable_ledger, retire_disable_encrypted_wal,
    unwrap_disable_index_files, write_disable_ledger,
};
use crate::encryption::config::KeyProviderConfig;
use crate::encryption::factory::Algorithm;

/// Outcome of a completed encrypted → plaintext disable migration.
///
/// Each flag reports whether that at-rest layer's unwrap pass ran (the layer was
/// in scope). `wal_migrated`, `index_migrated`, and `checkpoint_migrated` are
/// always `true` for a durable database; `cold_migrated` is `true` only when a
/// cold tier was present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisableReport {
    /// WAL segments stripped to the plaintext (v13) format.
    pub wal_migrated: bool,
    /// `AEIX` index files unwrapped back to bare plaintext.
    pub index_migrated: bool,
    /// Checkpoint files covered (ride the index DEK/format unwrap pass).
    pub checkpoint_migrated: bool,
    /// Cold-storage `ACV1` values unwrapped back to bare (only when a cold tier is
    /// present; `false` otherwise).
    pub cold_migrated: bool,
}

/// Map the WAL seam's **not-installed** rejection to a `FailedPrecondition` —
/// disabling an already-plaintext WAL is a caller precondition failure, not an
/// internal fault. Only the distinguishable
/// [`StorageError::WalKeyringNotInstalled`] variant is reclassified; a genuine WAL
/// I/O / seal-and-reopen fault is passed through UNCHANGED so it keeps its
/// `INTERNAL` classification.
fn map_wal_uninstall_err(e: Error) -> Error {
    match &e {
        Error::Storage(StorageError::WalKeyringNotInstalled { reason }) => {
            Error::FailedPrecondition(format!(
                "cannot disable encryption: the WAL is already plaintext ({reason})"
            ))
        }
        _ => e,
    }
}

/// The durable "encryption disabled" authority a completed disable writes.
fn disabled_state() -> EncryptionState {
    EncryptionState {
        enabled: false,
        key_source: None,
        algorithm: None,
    }
}

impl AletheiaDB {
    /// The AEAD algorithm the on-disk ciphertext was written under, which the
    /// unwrap passes must decrypt with (Issue #3616 PR4). Sourced from
    /// `config.encryption.algorithm` (retained as
    /// [`configured_encryption_algorithm`](AletheiaDB::configured_encryption_algorithm)),
    /// which on the encrypted-reopen path is the authority-pinned concrete
    /// algorithm — the exact cipher the data was encrypted with.
    fn disable_algorithm(&self) -> Algorithm {
        self.configured_encryption_algorithm
    }

    /// Migrate this **encrypted** database back to plaintext-at-rest, in place,
    /// crash-consistently (Issue #3616 PR4) — the inverse of
    /// [`enable_encryption`](AletheiaDB::enable_encryption).
    ///
    /// # ⚠️ LOUD REOPEN CONTRACT — read before calling
    ///
    /// Like `enable_encryption`, this is a **reopen-centric** migration. When it
    /// returns `Ok`, the handle you hold is in a deliberately **partial**,
    /// **quiesced** runtime state:
    ///
    /// * **WAL** is plaintext **live** — the keyring has been uninstalled and every
    ///   subsequent WAL append is written plaintext.
    /// * **Index / checkpoint / cold persistence is QUIESCED** — the background
    ///   index-persistence thread was stopped (and joined) as step 1 and is **not
    ///   restarted by this call**. This handle's index manager still carries the
    ///   ENCRYPTED keyring, so a persist through it would write `AEIX` over the
    ///   freshly-unwrapped plaintext files — the exact corruption this engine
    ///   prevents. [`persist_indexes`](AletheiaDB::persist_indexes) is therefore
    ///   **fail-closed** on a post-disable handle (the WAL is now plaintext but the
    ///   manager keyring is still `Some`). **You MUST reopen the database** (drop
    ///   this handle and call [`AletheiaDB::open`]) to get a fully-operational
    ///   plaintext instance: the next `open()` reads the flipped authority, brings
    ///   every layer up plaintext, and restarts the background persistence thread.
    ///
    /// # Errors
    ///
    /// Returns a structured error (never a partial silent success) when:
    /// * index persistence is not enabled (an ephemeral database cannot be
    ///   migrated — `FailedPrecondition`);
    /// * the database is not encrypted (`FailedPrecondition` — nothing to disable);
    /// * the current key source is a Passphrase/KMS/Vault backend (the ledger can
    ///   only persist a File/Env reference — `FailedPrecondition`);
    /// * any layer unwrap (WAL / index / checkpoint / cold) or the durable
    ///   authority flip fails.
    ///
    /// On any error the binding order guarantees the database is left either fully
    /// encrypted (authority never flipped) or resumable via
    /// [`resume_pending_disable`](AletheiaDB::resume_pending_disable) on the next
    /// `open()` — never a silently half-plaintext state that reads as encrypted.
    pub fn disable_encryption(&mut self) -> Result<DisableReport> {
        // === Step 0: preconditions (no side effects — run BEFORE quiesce) ============
        let manager = self.persistence_manager.clone().ok_or_else(|| {
            Error::FailedPrecondition(
                "cannot disable encryption on an ephemeral (in-memory) database: no durable \
                 at-rest bytes or authority file to migrate. Open a durable database first."
                    .to_string(),
            )
        })?;

        if self.encryption_manager.is_none() && !self.wal.is_encrypted() {
            return Err(Error::FailedPrecondition(
                "database is not encrypted; nothing to disable".to_string(),
            ));
        }

        // The current key source + version protect the on-disk ciphertext; record
        // them so a resumed disable can rebuild the decrypt ciphers.
        let enc_cfg = self.encryption_config.clone().ok_or_else(|| {
            Error::FailedPrecondition(
                "database reports encrypted but has no retained encryption config to source the \
                 current key from; cannot disable"
                    .to_string(),
            )
        })?;
        let current_source = enc_cfg.key_provider.clone();
        // Only File/Env sources round-trip through the ledger (they name a
        // reference, not a secret). A secret-backed source cannot be recorded, so a
        // resumed disable could not rebuild the decrypt cipher — refuse up front.
        match &current_source {
            KeyProviderConfig::File { .. } | KeyProviderConfig::Env { .. } => {}
            other => {
                let (provider_type, _) = other.describe();
                return Err(Error::FailedPrecondition(format!(
                    "disabling encryption with a {provider_type} key source is not supported \
                     (only file/env references can be persisted without leaking a secret)"
                )));
            }
        }

        let algorithm = self.disable_algorithm();
        // The generation the on-disk ciphertext is stamped with (the WAL keyring's
        // current version; falls back to the base version if unavailable).
        let current_version = self
            .wal
            .wal_keyring()
            .map(|k| k.current_version())
            .unwrap_or(crate::db::rotation::ENABLE_KEY_VERSION);

        // Is a cold tier in scope? Its `ACV1` values are unwrapped to bare below.
        let cold_in_scope = self.historical.read().has_tiered_storage();

        // === Step 1: quiesce the background index-persistence thread =================
        // Stop + join while the whole DB is still encrypted, so the worker's
        // shutdown final-persist is a safe encrypted write. NOT restarted here (see
        // the reopen contract) — respawn is the reopen's fresh worker.
        if let Some(tracker) = self.persistence_tracker.as_ref() {
            tracker.signal_shutdown();
        }
        if let Some(handle) = self.persistence_thread_handle.take() {
            let _ = handle.join();
        }

        // === Step 1b: synchronous full persist while STILL ENCRYPTED =================
        // Capture ALL current state into the durable (`AEIX`) index snapshot NOW,
        // before the WAL is uninstalled + its encrypted segments retired. The index
        // unwrap below turns this snapshot into plaintext, so afterwards the
        // PLAINTEXT snapshot holds every record — which is what makes it safe to
        // RETIRE the encrypted WAL segments without data loss (reopen reconstructs
        // from the plaintext snapshot, not the deleted encrypted WAL). Runs while
        // the WAL is still encrypted, so the fail-closed post-disable `persist_indexes`
        // guard (WAL plaintext + manager keyring Some) does not apply.
        self.persist_indexes()?;

        // === Step 2: durable ledger (breadcrumb #1) then migrate every layer =========
        // WAL is always Pending; index + checkpoint are Pending (a durable database
        // always has an index dir to unwrap); cold is Pending iff a cold tier exists.
        write_disable_ledger(
            &manager,
            &current_source,
            current_version,
            true,
            cold_in_scope,
        )?;

        // WAL: uninstall the keyring (seal encrypted v16 → store None → reopen
        // plaintext v13) via the slice-2 seam, then record completion (breadcrumb #2).
        self.wal
            .uninstall_wal_keyring()
            .map_err(map_wal_uninstall_err)?;
        mark_wal_complete(&manager)?;

        // WAL encrypted retire: now that the durable (`AEIX`) index snapshot holds
        // every record (Step 1b), retire the sealed ENCRYPTED (v16) WAL segments so
        // no undecryptable ciphertext survives at rest (breadcrumb #2b). Idempotent +
        // resumable: a crash mid-retire leaves `wal_retire=Pending`, and the resume
        // re-runs it. The fresh plaintext (v13) active segment is kept.
        retire_disable_encrypted_wal(&self.wal)?;
        mark_wal_retire_complete(&manager)?;

        // Index + checkpoint: unwrap every `AEIX` index file back to bare plaintext
        // under the index DEK, then record completion (breadcrumb #3).
        unwrap_disable_index_files(&manager, &current_source, algorithm)?;
        mark_index_complete(&manager)?;

        // Cold: unwrap every `ACV1` stored value back to bare on the live (encrypted)
        // cold store, then record completion (breadcrumb #4). Byte-preserving; full
        // plaintext cold reads resume at the next open().
        if cold_in_scope {
            let tiered = self.historical.read().tiered_storage_arc().ok_or_else(|| {
                Error::FailedPrecondition(
                    "cold tier vanished between precondition check and migration".to_string(),
                )
            })?;
            tiered.cold_storage().unwrap_encrypted_cold_values()?;
            mark_cold_complete(&manager)?;
        }

        // === Step 3: flip the authority to DISABLED BEFORE clearing (binding order) ==
        // (breadcrumb #5) A crash after this but before the clear resumes via
        // `resume_pending_disable`. The authority stayed `enabled` through every
        // unwrap above so a mid-disable crash could still decrypt; flipping it to
        // `disabled` is the commit point.
        write_encryption_state_durable(manager.base_path(), &disabled_state())?;

        // === Step 4: clear the ledger (breadcrumb #6) ================================
        clear_rotation_state(&manager);

        Ok(DisableReport {
            wal_migrated: true,
            index_migrated: true,
            checkpoint_migrated: true,
            cold_migrated: cold_in_scope,
        })
    }

    /// Resume an interrupted encrypted → plaintext disable migration at `open()`
    /// time, if one is pending (Issue #3616 PR4) — the mirror of
    /// [`resume_pending_enable`](AletheiaDB::resume_pending_enable).
    ///
    /// By the time this runs at startup the pre-replay
    /// [`install_pending_disable_wal_keyring`](crate::db::rotation::install_pending_disable_wal_keyring)
    /// hook has already installed the decrypt keyring when the on-disk WAL was still
    /// encrypted, so the replay read decrypted correctly; here we finish any
    /// still-pending WAL uninstall + retire, unwrap the index inline (BEFORE the
    /// index directory is read back), and — once every non-WAL layer is settled and
    /// there is no cold work to defer — flip the authority to `disabled` and clear
    /// the ledger. Cold is deferred to
    /// [`resume_pending_disable_cold`](AletheiaDB::resume_pending_disable_cold),
    /// which runs after the cold store is wired.
    ///
    /// **Guarded no-op:** returns `Ok(())` immediately when no durable
    /// pending-disable ledger exists (the common startup path).
    pub(crate) fn resume_pending_disable(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_disable_ledger(manager)? else {
            return Ok(());
        };
        let algorithm = self.disable_algorithm();

        // WAL: ensure the live WAL is plaintext, then retire the encrypted tail.
        // The pre-read hook may have (re-)installed the decrypt keyring so the
        // replay read could decrypt; if the WAL is currently encrypted, uninstall it
        // now so the end state is plaintext. Driven by `wal_retire` not yet
        // Complete, so a crash after a full retire is a clean no-op here.
        if view.wal_encrypted_on_disk() {
            if self.wal.is_encrypted() {
                self.wal
                    .uninstall_wal_keyring()
                    .map_err(map_wal_uninstall_err)?;
            }
            mark_wal_complete(manager)?;
            // Lossless: the original disable ran a synchronous persist BEFORE
            // writing this ledger, so the durable index snapshot holds every record,
            // and startup already read every WAL segment into memory
            // (`startup_wal_entries`) before this runs — deleting the encrypted
            // segment FILES here cannot lose an in-memory entry.
            retire_disable_encrypted_wal(&self.wal)?;
            mark_wal_retire_complete(manager)?;
        }

        // Index + checkpoint: complete the `AEIX` → plaintext unwrap pass INLINE,
        // BEFORE the caller reads the index directory back (`open()` runs this before
        // `load_indexes`). Idempotent — a crash mid-pass left a mix of `AEIX` +
        // plaintext files, and re-running unwraps only the remaining `AEIX` ones. The
        // manager was built PLAINTEXT on this startup (the disable end state), so the
        // subsequent index load reads the now-bare files correctly.
        if view.index_pending || view.checkpoint_pending {
            unwrap_disable_index_files(manager, &view.current_source, algorithm)?;
            mark_index_complete(manager)?;
        }

        // Re-read the ledger to reflect the completions just recorded, then decide on
        // the flip from the FRESH per-field state. If any non-WAL layer is still
        // `Pending` it can only be cold: the cold store is not wired at this point in
        // `open()` (it is constructed AFTER index load), so DEFER the cold unwrap +
        // the authority flip + the ledger clear to `resume_pending_disable_cold`.
        // Leaving the ledger present is the binding-order guarantee: the authority is
        // not flipped until every layer, cold included, is settled.
        let Some(fresh) = read_disable_ledger(manager)? else {
            return Ok(());
        };
        if !fresh.non_wal_layers_settled() {
            return Ok(());
        }

        // Every non-WAL layer is settled and there is no cold work to defer: flip the
        // authority to `disabled` BEFORE clearing the ledger (idempotent if already
        // flipped — the flip→clear-gap resume case).
        write_encryption_state_durable(manager.base_path(), &disabled_state())?;
        clear_rotation_state(manager);
        Ok(())
    }

    /// Finish an interrupted disable migration's COLD layer at `open()` time, once
    /// the cold store has been wired onto `self.historical` (Issue #3616 PR4) — the
    /// mirror of [`resume_pending_enable_cold`](AletheiaDB::resume_pending_enable_cold).
    ///
    /// [`resume_pending_disable`](AletheiaDB::resume_pending_disable) runs before the
    /// cold tier is constructed, so it cannot unwrap cold values; it DEFERS the cold
    /// unwrap, the authority flip, and the ledger clear to this method. This
    /// preserves the binding order — the authority is flipped to `disabled` only
    /// after **every** layer (cold included) is settled.
    ///
    /// The cold store is built under the disable decrypt cold DEK on this startup
    /// (see [`disable_resume_ciphers`](crate::db::rotation::disable_resume_ciphers)),
    /// so [`unwrap_encrypted_cold_values`](crate::storage::redb_cold_storage::RedbColdStorage::unwrap_encrypted_cold_values)
    /// can decrypt each `ACV1` value and rewrite it bare. Idempotent / resumable: an
    /// already-bare value is skipped.
    ///
    /// # ⚠️ Cold reads on the resuming session require one more reopen
    ///
    /// Unlike the WAL and index layers (whose resumed session is fully usable), the
    /// COLD layer has a sharp edge: this method unwraps the `ACV1` values to bare on
    /// a cold store that STILL CARRIES the decrypt keyring (there is no live
    /// `Some → None` cold-keyring uninstall). The cold read path decrypts by keyring
    /// presence — a bare value read through a keyring'd store is treated as legacy
    /// bare-ciphertext and fails to decrypt. So while this method durably completes
    /// the disable (values bare on disk, authority flipped to `disabled`, ledger
    /// cleared), **cold value reads on THIS resuming session will fail** until the
    /// database is reopened once more — the next `open()` reads the `disabled`
    /// authority and builds a PLAINTEXT cold store that reads the bare values
    /// correctly. This mirrors the driver's loud reopen contract; a crashed disable
    /// with a cold tier therefore wants one extra reopen to be fully usable. (WAL /
    /// index resumes need no such extra reopen.)
    ///
    /// **Guarded no-op:** returns `Ok(())` when no pending-disable ledger exists or
    /// its cold layer is not `Pending`.
    pub(crate) fn resume_pending_disable_cold(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_disable_ledger(manager)? else {
            return Ok(());
        };
        if !view.cold_pending {
            return Ok(());
        }

        // Cold is Pending but the store is not wired: the ledger claims a cold tier
        // that this open() did not construct. Fail closed rather than flip the
        // authority while `ACV1` values remain encrypted under a to-be-dropped key.
        let tiered = self.historical.read().tiered_storage_arc().ok_or_else(|| {
            Error::FailedPrecondition(
                "pending disable ledger records a cold layer, but no cold tier is configured on \
                 this open(); reopen with the cold tier enabled so the migration can complete"
                    .to_string(),
            )
        })?;
        tiered.cold_storage().unwrap_encrypted_cold_values()?;
        mark_cold_complete(manager)?;

        // Every layer is now settled: binding order — flip the authority BEFORE
        // clearing the ledger (idempotent if already flipped).
        write_encryption_state_durable(manager.base_path(), &disabled_state())?;
        clear_rotation_state(manager);
        Ok(())
    }

    /// Fail LOUDLY at startup when an interrupted disable recorded a cold layer as
    /// `Pending` but this `open()` will NOT construct a cold tier (Issue #3616 PR4)
    /// — the mirror of
    /// [`fail_if_pending_enable_cold_without_tier`](AletheiaDB::fail_if_pending_enable_cold_without_tier).
    ///
    /// Converts the silent-limbo case (cold still `Pending` →
    /// `non_wal_layers_settled() == false` → authority never flips) into an
    /// actionable `FailedPrecondition`. Called on the startup branch that does NOT
    /// build a cold tier. A guarded no-op when no pending-disable ledger exists or
    /// its cold layer is not `Pending`.
    pub(crate) fn fail_if_pending_disable_cold_without_tier(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_disable_ledger(manager)? else {
            return Ok(());
        };
        if view.cold_pending {
            return Err(Error::FailedPrecondition(
                "pending disable ledger records a cold layer, but no cold tier is configured on \
                 this open(); reopen with the cold tier enabled so the migration can complete"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::db::encryption_state::{encryption_state_path, read_encryption_state};
    use crate::db::rotation::{mark_wal_complete, mark_wal_retire_complete, write_disable_ledger};
    use crate::encryption::config::{EncryptionConfig, KeyProviderConfig};
    use crate::encryption::key_provider::FileKeyProvider;
    use crate::storage::index_persistence::{IndexPersistenceManager, PersistenceConfig};
    use crate::storage::wal::DurabilityMode;
    use crate::{AletheiaDB, PropertyMapBuilder};
    use std::path::Path;
    use std::sync::Arc;

    fn key_source_at(dir: &Path) -> KeyProviderConfig {
        let key_path = dir.join("mek.key");
        FileKeyProvider::generate_key_file(&key_path).expect("generate key file");
        KeyProviderConfig::File { path: key_path }
    }

    /// A durable, ENCRYPTED config rooted at `data_dir`, keyed by `key`.
    fn encrypted_durable_config(data_dir: &Path, key: &KeyProviderConfig) -> AletheiaDBConfig {
        let key_file = match key {
            KeyProviderConfig::File { path } => path.clone(),
            _ => panic!("test helper expects a File key source"),
        };
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
            .encryption(EncryptionConfig::file_based(key_file))
            .build()
    }

    /// Like [`encrypted_durable_config`] but WITH an encrypted cold (redb) tier, so
    /// the disable cold `ACV1` → bare unwrap pass is exercised.
    fn encrypted_durable_config_with_cold(
        data_dir: &Path,
        key: &KeyProviderConfig,
    ) -> AletheiaDBConfig {
        use crate::config::HistoricalConfigBuilder;
        use std::time::Duration;
        let key_file = match key {
            KeyProviderConfig::File { path } => path.clone(),
            _ => panic!("test helper expects a File key source"),
        };
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
            .historical(
                HistoricalConfigBuilder::new()
                    .enable_cold_storage(true)
                    .cold_storage_path(data_dir.join("cold.redb"))
                    .migration_age_threshold(Duration::from_secs(3600))
                    .build(),
            )
            .encryption(EncryptionConfig::file_based(key_file))
            .build()
    }

    /// Seed a node version directly into the ENCRYPTED cold store of `db`, which
    /// wraps it to `ACV1`. Returns the version-id `u64`.
    fn seed_encrypted_cold_node(db: &AletheiaDB, vid_u64: u64, node_id: u64, name: &str) -> u64 {
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::version::NodeVersion;
        let cold_vid = crate::core::id::VersionId::new(vid_u64).unwrap();
        let node = NodeVersion::new_anchor(
            cold_vid,
            crate::core::NodeId::new(node_id).unwrap(),
            crate::core::temporal::BiTemporalInterval::current(1234.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            PropertyMapBuilder::new().insert("name", name).build(),
        );
        let tiered = db
            .historical
            .read()
            .tiered_storage_arc()
            .expect("cold tier configured");
        let cold = tiered.cold_storage();
        assert!(cold.is_encrypted(), "seed store is encrypted");
        cold.store_node_version(&node).unwrap();
        assert!(
            cold.raw_node_value_is_acv1_for_test(vid_u64),
            "seeded cold value must be ACV1 (encrypted) before disable"
        );
        vid_u64
    }

    /// The index-files subdirectory the classify helper walks.
    fn index_files_dir(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("indexes").join("indexes")
    }

    fn manager_for(data_dir: &Path) -> Arc<IndexPersistenceManager> {
        Arc::new(IndexPersistenceManager::new(data_dir.join("indexes")))
    }

    /// Count index files under `dir` -> (total, plaintext_count, aeix_count).
    fn classify_index_files(dir: &Path) -> (usize, usize, usize) {
        use crate::storage::index_persistence::common::is_encrypted_index;
        fn walk(dir: &Path, total: &mut usize, plain: &mut usize, aeix: &mut usize) {
            if !dir.exists() {
                return;
            }
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, total, plain, aeix);
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".tmp")
                    || name.contains(".tmp.")
                    || name.starts_with(".aeix-usearch-tmp-")
                {
                    continue;
                }
                *total += 1;
                let bytes = std::fs::read(&p).unwrap();
                if is_encrypted_index(&bytes) {
                    *aeix += 1;
                } else {
                    *plain += 1;
                }
            }
        }
        let (mut total, mut plain, mut aeix) = (0, 0, 0);
        walk(dir, &mut total, &mut plain, &mut aeix);
        (total, plain, aeix)
    }

    /// Any `.log` WAL segment whose header is encrypted (v16 keyversioned).
    fn any_encrypted_wal_segment(wal_dir: &Path) -> bool {
        use crate::storage::wal::segment_reader::max_key_version_in_dir;
        // A plaintext WAL directory reports no key version; an encrypted one does.
        max_key_version_in_dir(wal_dir).is_some()
    }

    /// Happy path: disable strips the WAL + index encryption, flips the authority to
    /// disabled, clears the ledger; the reopened database reads its data back as
    /// plaintext and no encrypted files remain.
    #[test]
    fn disable_encryption_strips_wal_index_flips_authority_clears_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());
        let wal_dir = dir.path().join("wal");

        {
            let mut db =
                AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                    .unwrap();
            db.create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
                .unwrap();
            db.create_node("Person", PropertyMapBuilder::new().insert("n", "b").build())
                .unwrap();
            assert!(db.wal.is_encrypted(), "starts encrypted");

            let report = db.disable_encryption().unwrap();
            assert!(report.wal_migrated);
            assert!(report.index_migrated && report.checkpoint_migrated);
            assert!(!report.cold_migrated, "no cold tier in this config");
            assert!(!db.wal.is_encrypted(), "WAL live-plaintext after disable");

            // Authority flipped to disabled on disk.
            let state = read_encryption_state(&indexes).unwrap().unwrap();
            assert!(!state.enabled);
            assert_eq!(state.key_source, None);
            // Ledger cleared.
            assert!(!indexes.join("rotation.state").exists());
        }

        // Every persisted index file is now plaintext; no encrypted WAL remains.
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(total > 0, "disable persisted an index snapshot to sweep");
        assert_eq!(
            aeix, 0,
            "no AEIX index file survives disable (whole-dir sweep)"
        );
        assert_eq!(
            plain, total,
            "every persisted index file is plaintext after disable"
        );
        assert!(
            !any_encrypted_wal_segment(&wal_dir),
            "no encrypted WAL segment survives disable"
        );

        // Reopen under the flipped authority: data survives, WAL is plaintext.
        let db =
            AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key)).unwrap();
        assert_eq!(db.node_count(), 2, "nodes survive reopen as plaintext");
        assert!(
            !db.wal.is_encrypted(),
            "reopen honors the disabled authority"
        );
        // The reopened plaintext instance is fully operational.
        db.create_node("Person", PropertyMapBuilder::new().insert("n", "c").build())
            .unwrap();
        assert_eq!(db.node_count(), 3);
    }

    /// Disabling an ephemeral (in-memory) database is refused.
    #[test]
    fn disable_encryption_refuses_ephemeral() {
        let mut db = AletheiaDB::new().unwrap();
        let err = db.disable_encryption().unwrap_err();
        assert!(matches!(
            err,
            crate::core::error::Error::FailedPrecondition(_)
        ));
    }

    /// Disabling a plaintext (never-encrypted) database is refused.
    #[test]
    fn disable_encryption_refuses_when_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.path().join("wal"))
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: dir.path().join("indexes"),
                load_on_startup: true,
                ..Default::default()
            })
            .build();
        let mut db = AletheiaDB::with_unified_config(config).unwrap();
        let err = db.disable_encryption().unwrap_err();
        assert!(matches!(
            err,
            crate::core::error::Error::FailedPrecondition(_)
        ));
    }

    /// A1: `map_wal_uninstall_err` reclassifies ONLY the distinguishable
    /// not-installed rejection to `FailedPrecondition`; a genuine WAL I/O fault is
    /// passed through unchanged (INTERNAL-class), never mislabeled.
    #[test]
    fn map_wal_uninstall_err_distinguishes_precondition_from_io_fault() {
        use crate::core::error::{Error, StorageError};

        let not_installed = Error::Storage(StorageError::WalKeyringNotInstalled {
            reason: "no WAL keyring is installed".to_string(),
        });
        match super::map_wal_uninstall_err(not_installed) {
            Error::FailedPrecondition(msg) => {
                assert!(msg.contains("already plaintext"), "got: {msg}");
            }
            other => panic!("expected FailedPrecondition, got: {other:?}"),
        }

        let io_fault = Error::Storage(StorageError::WalError {
            reason: "failed to fsync sealed segment: disk full".to_string(),
        });
        match super::map_wal_uninstall_err(io_fault) {
            Error::Storage(StorageError::WalError { reason }) => {
                assert!(reason.contains("disk full"), "genuine I/O fault preserved");
            }
            other => panic!("I/O WalError must not be reclassified, got: {other:?}"),
        }
    }

    /// C-WAL: crash AFTER the disable ledger but BEFORE any layer work — the WAL is
    /// still encrypted AND the index dir is still `AEIX`. This models the REAL
    /// ledger shape a durable disable writes: `wal=Pending` AND `index=Pending`.
    /// Reopen must resume BOTH layers: uninstall+retire the WAL, unwrap the index to
    /// plaintext, and only THEN flip the authority to disabled + clear the ledger.
    #[test]
    fn disable_crash_after_ledger_before_work_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());
        let wal_dir = dir.path().join("wal");

        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                .unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("n", "x").build())
                .unwrap();
            // Force an encrypted index snapshot to disk so the index dir is a genuine
            // AEIX layer the resume must unwrap.
            db.persist_indexes().unwrap();
        }
        // Precondition: index dir is AEIX, WAL is encrypted.
        let (total, _plain, aeix) = classify_index_files(&idx_dir);
        assert!(total > 0 && aeix == total, "index starts fully AEIX");
        assert!(any_encrypted_wal_segment(&wal_dir), "WAL starts encrypted");

        // Simulate the interrupted state: a fresh disable ledger (wal=Pending AND
        // index=Pending), all bytes still encrypted, authority still enabled.
        write_disable_ledger(
            &manager_for(dir.path()),
            &key,
            crate::db::rotation::ENABLE_KEY_VERSION,
            true,
            false,
        )
        .unwrap();
        assert!(indexes.join("rotation.state").exists());

        // Reopen: resume completes BOTH the WAL strip+retire and the index unwrap.
        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                .unwrap();
            assert_eq!(
                db.node_count(),
                1,
                "encrypted bytes decrypted then unwrapped"
            );
            assert!(
                !db.wal.is_encrypted(),
                "resume stripped the WAL to plaintext"
            );
        }
        // Index fully plaintext, no encrypted WAL, authority disabled, ledger cleared.
        let (total2, plain2, aeix2) = classify_index_files(&idx_dir);
        assert_eq!(aeix2, 0, "resume unwrapped every AEIX index file");
        assert_eq!(plain2, total2, "index dir is fully plaintext after resume");
        assert!(
            !any_encrypted_wal_segment(&wal_dir),
            "resume retired every encrypted WAL segment"
        );
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(!state.enabled, "authority flipped to disabled after unwrap");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C-STORAGE: crash AFTER the WAL strip+retire (wal=Complete, wal_retire=Complete)
    /// but BEFORE the index unwrap and authority flip. Reopen must finish the index
    /// unwrap and flip + clear. Models a crash between the WAL and index stages.
    #[test]
    fn disable_crash_after_wal_before_index_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());

        // Run a REAL disable to reach the fully-plaintext, authority-disabled state,
        // then rewind to "after WAL strip+retire, before index unwrap/authority flip".
        {
            let mut db =
                AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                    .unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("n", "x").build())
                .unwrap();
            db.disable_encryption().unwrap();
        }
        // Rewind: remove the disabled authority (so config sees the encrypted config
        // as authoritative again), and re-lay a wal=Complete + wal_retire=Complete
        // disable ledger with index still Pending. The WAL + index are ALREADY
        // plaintext on disk from the real disable, so the resume WAL block is a no-op
        // (wal_retire already Complete) and the index unwrap is an idempotent skip —
        // but the ledger shape drives resume to flip + clear.
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        assert!(read_encryption_state(&indexes).unwrap().is_none());
        {
            let mgr = manager_for(dir.path());
            write_disable_ledger(
                &mgr,
                &key,
                crate::db::rotation::ENABLE_KEY_VERSION,
                true,
                false,
            )
            .unwrap();
            mark_wal_complete(&mgr).unwrap();
            mark_wal_retire_complete(&mgr).unwrap();
        }
        assert!(indexes.join("rotation.state").exists());

        // Reopen: config sees the encrypted config (no disabled authority) but a
        // pending disable ledger forces the plaintext build; resume finishes.
        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                .unwrap();
            assert_eq!(db.node_count(), 1);
            assert!(!db.wal.is_encrypted());
        }
        let (_total, _plain, aeix) = classify_index_files(&idx_dir);
        assert_eq!(aeix, 0, "index dir is plaintext after resume");
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(!state.enabled, "authority flipped to disabled by resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C-CLEAR: crash in the flip→clear gap (authority already `disabled`, ledger
    /// still present, everything already plaintext). Reopen: the rotation resume
    /// paths skip the disable ledger; only `resume_pending_disable` clears it.
    #[test]
    fn disable_crash_after_authority_before_clear_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        {
            let mut db =
                AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                    .unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("n", "x").build())
                .unwrap();
            db.disable_encryption().unwrap();
        }
        // Re-lay a fully-settled disable ledger, leaving the authority DISABLED.
        {
            let mgr = manager_for(dir.path());
            write_disable_ledger(
                &mgr,
                &key,
                crate::db::rotation::ENABLE_KEY_VERSION,
                true,
                false,
            )
            .unwrap();
            mark_wal_complete(&mgr).unwrap();
            mark_wal_retire_complete(&mgr).unwrap();
            crate::db::rotation::mark_index_complete(&mgr).unwrap();
        }
        assert!(indexes.join("rotation.state").exists());
        assert!(!read_encryption_state(&indexes).unwrap().unwrap().enabled);

        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                .unwrap();
            assert_eq!(db.node_count(), 1);
        }
        assert!(
            !indexes.join("rotation.state").exists(),
            "flip->clear gap resume cleared the ledger"
        );
        assert!(!read_encryption_state(&indexes).unwrap().unwrap().enabled);
    }

    /// COLD driver round-trip: an encrypted cold value is unwrapped to bare by the
    /// disable driver, and a reopen (plaintext cold) reads the identical record.
    #[test]
    fn disable_cold_acv1_to_bare_roundtrip() {
        use crate::core::version::EntityVersion;
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let cold_vid = 9101u64;

        {
            let mut db = AletheiaDB::with_unified_config(encrypted_durable_config_with_cold(
                dir.path(),
                &key,
            ))
            .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_encrypted_cold_node(&db, cold_vid, 500, "Carol");
            db.persist_indexes().unwrap();

            let report = db.disable_encryption().unwrap();
            assert!(report.cold_migrated, "cold tier in scope → migrated");

            // On-disk cold value is now BARE (no ACV1 wrapper).
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            assert!(
                !tiered
                    .cold_storage()
                    .raw_node_value_is_acv1_for_test(cold_vid),
                "cold value unwrapped to bare after disable"
            );
        }

        // Reopen under the flipped (disabled) authority: the cold store is built
        // plaintext, the bare value reads back as the identical record.
        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config_with_cold(
                dir.path(),
                &key,
            ))
            .unwrap();
            assert!(
                !db.wal.is_encrypted(),
                "reopen honors the disabled authority"
            );
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            let cold = tiered.cold_storage();
            assert!(!cold.is_encrypted(), "cold tier built plaintext on reopen");
            assert!(
                !cold.raw_node_value_is_acv1_for_test(cold_vid),
                "cold value stays bare across reopen"
            );
            let loaded = cold
                .get_node_version(crate::core::id::VersionId::new(cold_vid).unwrap())
                .unwrap()
                .expect("bare cold value reads on reopen");
            assert_eq!(loaded.version_id().as_u64(), cold_vid);
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(!state.enabled);
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C-COLD crash: crash right after the disable ledger (cold in scope) with every
    /// layer — WAL, index, AND cold — still encrypted. Reopen must resume ALL layers:
    /// strip+retire the WAL, unwrap the index, unwrap the cold `ACV1` values (via
    /// `resume_pending_disable_cold`, after the cold store is wired under the decrypt
    /// DEK), then flip the authority to disabled + clear the ledger.
    #[test]
    fn disable_cold_crash_after_ledger_resumes() {
        use crate::core::version::EntityVersion;
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let cold_vid = 9202u64;

        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config_with_cold(
                dir.path(),
                &key,
            ))
            .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_encrypted_cold_node(&db, cold_vid, 500, "Carol");
            db.persist_indexes().unwrap();
        }
        // Simulate a crash right after the ledger was written: WAL + index + cold all
        // Pending, all bytes still encrypted, authority still enabled.
        write_disable_ledger(
            &manager_for(dir.path()),
            &key,
            crate::db::rotation::ENABLE_KEY_VERSION,
            true, // index in scope
            true, // cold in scope
        )
        .unwrap();
        assert!(indexes.join("rotation.state").exists());

        // Reopen #1 (resume): completes WAL + index inline, defers cold to after the
        // cold store is wired, then unwraps cold + flips + clears the ledger. The
        // on-disk cold value is now BARE (checked via the raw read, which does not
        // decrypt). NB: cold VALUE reads on this resuming session would fail — the
        // cold store still carries the decrypt keyring (see the cold-resume sharp
        // edge on `resume_pending_disable_cold`) — so we only assert the durable
        // (raw + authority + ledger) state here.
        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config_with_cold(
                dir.path(),
                &key,
            ))
            .unwrap();
            assert!(!db.wal.is_encrypted(), "resume stripped the WAL");
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            assert!(
                !tiered
                    .cold_storage()
                    .raw_node_value_is_acv1_for_test(cold_vid),
                "resume unwrapped the cold value to bare on disk"
            );
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(
            !state.enabled,
            "authority flipped to disabled after cold unwrap"
        );
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");

        // Reopen #2 (clean, authority=disabled → plaintext cold): the bare cold value
        // now reads back as the identical record — the one-extra-reopen the cold
        // resume contract documents.
        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config_with_cold(
                dir.path(),
                &key,
            ))
            .unwrap();
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            let cold = tiered.cold_storage();
            assert!(
                !cold.is_encrypted(),
                "cold tier built plaintext on clean reopen"
            );
            let loaded = cold
                .get_node_version(crate::core::id::VersionId::new(cold_vid).unwrap())
                .unwrap()
                .expect("bare cold value reads on clean reopen");
            assert_eq!(loaded.version_id().as_u64(), cold_vid);
        }
    }

    /// Persisting through a QUIESCED post-disable handle is fail-closed and does NOT
    /// corrupt the freshly-unwrapped plaintext snapshot (mirror of the enable guard).
    #[test]
    fn persist_on_quiesced_post_disable_handle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let mut db =
            AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key)).unwrap();
        db.create_node("N", PropertyMapBuilder::new().build())
            .unwrap();
        db.disable_encryption().unwrap();
        // WAL is now plaintext but this handle's index manager still carries the
        // encrypted keyring — a persist would write AEIX over plaintext. Refused.
        let err = db.persist_indexes().unwrap_err();
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "persist on a post-disable quiesced handle must be fail-closed, got: {err:?}"
        );
    }

    /// FIX 1 (Issue #3616 PR4 review): a disable that FAILS at the cold-unwrap step
    /// leaves the handle QUIESCED with WAL plaintext + index-manager keyring `Some` +
    /// index files ALREADY plaintext on disk + authority STILL `enabled` (never
    /// flipped) + a pending `direction=disable` ledger. On that errored handle a
    /// persist gated on the authority ALONE reads `enabled` and would WRONGLY rewrite
    /// `AEIX` over the plaintext snapshot. The guard must ALSO fail-close on the
    /// pending disable ledger — this test proves it does.
    #[test]
    fn persist_on_failed_disable_authority_still_enabled_is_refused() {
        use crate::db::encryption_state::{EncryptionState, write_encryption_state_durable};
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());

        let mut db =
            AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key)).unwrap();
        db.create_node("N", PropertyMapBuilder::new().insert("n", "x").build())
            .unwrap();

        // A real disable unwraps the index to PLAINTEXT on disk and leaves this handle
        // quiesced (WAL plaintext live, manager keyring still `Some`). This is the
        // simplest faithful way to reach a plaintext-index-on-disk + keyring-`Some`
        // handle; we then rewind the durable state to the errored PRE-flip shape.
        db.disable_encryption().unwrap();
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(
            total > 0 && aeix == 0 && plain == total,
            "index is fully plaintext on disk after the real disable"
        );

        // Rewind to the errored PRE-flip state: authority STILL `enabled` (exactly as
        // it is when the cold-unwrap step fails before the commit-point flip) and a
        // pending disable ledger with cold still Pending. Writing `enabled` DEFEATS the
        // old authority-only clause (`is_some_and(|s| !s.enabled)` is false), so ONLY
        // the new disable-ledger clause can fail-close here.
        write_encryption_state_durable(&indexes, &EncryptionState::enabled(key.clone())).unwrap();
        assert!(
            read_encryption_state(&indexes).unwrap().unwrap().enabled,
            "authority is enabled again (the errored pre-flip shape)"
        );
        write_disable_ledger(
            &manager_for(dir.path()),
            &key,
            crate::db::rotation::ENABLE_KEY_VERSION,
            true, // index in scope
            true, // cold in scope, still Pending (mirrors a cold-unwrap failure)
        )
        .unwrap();
        assert!(
            indexes.join("rotation.state").exists(),
            "disable ledger present"
        );

        // Persist through the errored handle is REFUSED (fail-closed on the disable
        // ledger, despite the authority reading `enabled`).
        let err = db.persist_indexes().unwrap_err();
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "persist on a failed-disable handle (authority still enabled + pending \
             disable ledger) must be fail-closed, got: {err:?}"
        );

        // And it wrote NO `AEIX` over the plaintext snapshot.
        let (total2, plain2, aeix2) = classify_index_files(&idx_dir);
        assert_eq!(aeix2, 0, "refused persist wrote no AEIX index file");
        assert_eq!(plain2, total2, "index dir stays fully plaintext");
        assert!(total2 >= total, "no index file was lost");
    }

    /// T1 (Issue #3616 PR4 review): end-to-end disable over a DB with EDGES and a
    /// VECTOR INDEX — the likeliest real-world data-loss surface, since adjacency
    /// bytes and usearch bytes must round-trip through the encrypted→plaintext
    /// migration. A reopen must read back every node, every edge, a traversal, AND
    /// `find_similar` as plaintext, with no `AEIX` index file or encrypted WAL segment
    /// surviving.
    #[test]
    fn disable_preserves_edges_and_vector_index_end_to_end() {
        use crate::db::similarity_query::SimilarityQuery;
        use crate::index::vector::{DistanceMetric, HnswConfig};
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let idx_dir = index_files_dir(dir.path());
        let wal_dir = dir.path().join("wal");

        // Clearly separated directions so cosine ordering is unambiguous: n1 is near
        // n0, n2 is orthogonal to n0.
        let emb_n0 = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let emb_n1 = vec![0.95f32, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let emb_n2 = vec![0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];

        let (n0, n1, n2);
        {
            let mut db =
                AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                    .unwrap();
            assert!(db.wal.is_encrypted(), "starts encrypted");

            db.vector_index("embedding")
                .hnsw(HnswConfig::new(8, DistanceMetric::Cosine))
                .enable()
                .unwrap();

            n0 = db
                .create_node(
                    "Doc",
                    PropertyMapBuilder::new()
                        .insert("t", "rust")
                        .insert_vector("embedding", &emb_n0)
                        .build(),
                )
                .unwrap();
            n1 = db
                .create_node(
                    "Doc",
                    PropertyMapBuilder::new()
                        .insert("t", "python")
                        .insert_vector("embedding", &emb_n1)
                        .build(),
                )
                .unwrap();
            n2 = db
                .create_node(
                    "Doc",
                    PropertyMapBuilder::new()
                        .insert("t", "go")
                        .insert_vector("embedding", &emb_n2)
                        .build(),
                )
                .unwrap();

            db.create_edge(
                n0,
                n1,
                "LINKS",
                PropertyMapBuilder::new().insert("w", "1").build(),
            )
            .unwrap();
            db.create_edge(n1, n2, "LINKS", PropertyMapBuilder::new().build())
                .unwrap();

            assert_eq!(db.node_count(), 3);
            assert_eq!(db.edge_count(), 2);
            assert!(
                !db.similarity_search(SimilarityQuery::from_node(n0).k(2))
                    .unwrap()
                    .is_empty(),
                "vector search works before disable (baseline)"
            );

            let report = db.disable_encryption().unwrap();
            assert!(report.wal_migrated && report.index_migrated);
            assert!(!db.wal.is_encrypted(), "WAL live-plaintext after disable");
        }

        // No `AEIX` index file, no encrypted WAL segment survives.
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(total > 0, "disable persisted an index snapshot");
        assert_eq!(aeix, 0, "no AEIX index file survives disable");
        assert_eq!(plain, total, "every index file is plaintext");
        assert!(
            !any_encrypted_wal_segment(&wal_dir),
            "no encrypted WAL segment survives disable"
        );

        // Reopen under the flipped authority: graph + vector index survive as plaintext.
        let db =
            AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key)).unwrap();
        assert!(
            !db.wal.is_encrypted(),
            "reopen honors the disabled authority"
        );
        assert_eq!(db.node_count(), 3, "nodes survive reopen");
        assert_eq!(db.edge_count(), 2, "edges survive reopen");

        // Traversal (adjacency) survives the round-trip.
        let out0 = db.get_outgoing_edges(n0);
        assert_eq!(out0.len(), 1, "n0 keeps its single outgoing edge");
        let e = db.get_edge(out0[0]).unwrap();
        assert_eq!(e.source, n0, "edge source preserved");
        assert_eq!(e.target, n1, "edge target preserved");
        assert_eq!(
            db.get_outgoing_edges(n1).len(),
            1,
            "n1 keeps its single outgoing edge"
        );

        // Vector index restored + `find_similar` works as plaintext, with the correct
        // nearest neighbor (proving the usearch bytes round-tripped, not just that the
        // index exists).
        assert!(
            db.is_vector_index_enabled_for("embedding"),
            "vector index restored on reopen"
        );
        let similar = db
            .similarity_search(SimilarityQuery::from_node(n0).k(2))
            .unwrap();
        assert!(
            !similar.is_empty(),
            "similarity_search works after disable + reopen"
        );
        // `from_node` excludes the query node, so the top hit is the true nearest
        // neighbor — proving the usearch bytes round-tripped, not just that the index
        // exists.
        assert_eq!(
            similar[0].0, n1,
            "nearest neighbor preserved through the plaintext round-trip"
        );
    }

    /// T2 (Issue #3616 PR4 review): `disable_encryption` refuses a secret-backed key
    /// source (Passphrase/KMS/Vault) up front — the ledger can only persist a
    /// non-secret File/Env reference, so a resumed disable could not rebuild the
    /// decrypt cipher. The refusal must be a `FailedPrecondition` that writes NO
    /// ledger (it precedes `write_disable_ledger`).
    #[test]
    fn disable_refuses_secret_backed_key_source_without_writing_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        let mut db =
            AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key)).unwrap();
        db.create_node("N", PropertyMapBuilder::new().build())
            .unwrap();

        // Swap the retained key-source reference to a secret-backed (Passphrase)
        // config. The refusal is provider-type-agnostic (any non-File/Env) and fires
        // BEFORE any provider is constructed, so no passphrase infrastructure is
        // needed — only the config value.
        let mut enc_cfg = db.encryption_config.clone().unwrap();
        enc_cfg.key_provider = KeyProviderConfig::PassphraseFile {
            path: dir.path().join("wrapped.key"),
            passphrase_env: "ALETHEIA_TEST_PASSPHRASE".to_string(),
        };
        db.encryption_config = Some(enc_cfg);

        let err = db.disable_encryption().unwrap_err();
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "secret-backed key source must be refused, got: {err:?}"
        );
        assert!(
            !indexes.join("rotation.state").exists(),
            "the refusal precedes the ledger write — no disable ledger is left behind"
        );
    }

    /// T3 (Issue #3616 PR4 review): the loud
    /// [`fail_if_pending_disable_cold_without_tier`] guard. A pending disable ledger
    /// records a `cold=Pending` layer, but a reopen configures NO cold tier — this
    /// would otherwise wedge (authority never flips, no diagnostic). The reopen must
    /// instead fail with an actionable `FailedPrecondition`.
    #[test]
    fn disable_pending_cold_without_tier_fails_loudly_not_wedged() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        {
            let db = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
                .unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("n", "x").build())
                .unwrap();
            db.persist_indexes().unwrap();
        }
        // Lay a disable ledger recording a cold layer as Pending (cold_in_scope=true),
        // simulating an interrupted disable of a cold-tier DB.
        write_disable_ledger(
            &manager_for(dir.path()),
            &key,
            crate::db::rotation::ENABLE_KEY_VERSION,
            true, // index in scope
            true, // cold in scope → Pending
        )
        .unwrap();
        assert!(indexes.join("rotation.state").exists());

        // Reopen WITHOUT a cold tier configured: the guard converts the silent-wedge
        // case into an actionable FailedPrecondition.
        let err = AletheiaDB::with_unified_config(encrypted_durable_config(dir.path(), &key))
            .expect_err("reopen with a pending cold-disable ledger but no cold tier must fail");
        match err {
            crate::core::error::Error::FailedPrecondition(msg) => {
                assert!(
                    msg.contains("cold tier"),
                    "error names the missing cold tier, got: {msg}"
                );
            }
            other => panic!("expected FailedPrecondition, got: {other:?}"),
        }
    }
}
