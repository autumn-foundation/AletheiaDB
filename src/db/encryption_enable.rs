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
//! 2. **Migrate every in-scope at-rest layer**, recording progress as a per-field
//!    [`LayerStatus`](crate::db::rotation) in a durable `direction=enable`
//!    rotation ledger so a crash mid-migration resumes idempotently:
//!    * **WAL** — install the DEK keyring via the PR2 seam (which seals the
//!      plaintext segment and force-rolls to a fresh encrypted v16 segment).
//!    * **Index + checkpoint** — a plaintext → `AEIX` wrap pass rewrites every
//!      bare index file under the index DEK (checkpoints ride the index DEK and
//!      format, so they are covered by the same pass).
//!    * **Cold** — a bare → `ACV1` wrap-only pass rewrites every stored cold value
//!      wrapped under the cold DEK (only when a cold tier is present).
//! 3. **Flip the [`encryption.state`] authority to `enabled` BEFORE clearing the
//!    ledger.** This binding order closes the crash gap: were the ledger cleared
//!    first and a crash struck before the authority flip, the next `open()` would
//!    read the unchanged plaintext config over now-encrypted bytes and mis-read
//!    (undecodable) ciphertext as plaintext. With the authority flipped first, a
//!    crash between the two steps leaves a still-present ledger that
//!    [`resume_pending_enable`] reconciles on the next `open()`.
//! 4. **Clear the ledger.**
//!
//! # In scope vs deferred
//!
//! In scope: **WAL**, **index**, **checkpoint**, and **cold**. Each layer's
//! migration is byte-preserving and reader-compatible on reopen (the index reader
//! header-sniffs `AEIX`; the cold reader dispatches on the `ACV1` wrapper).
//!
//! Deferred (NOT migrated by v1): `.albk` backups, `snapshots.json`, auth
//! `keys.json`, `schema_constraints.dat`.
//!
//! # Security caveats (defense-in-depth, documented)
//!
//! * **In-place overwrite does not shred freed plaintext blocks.** The index wrap
//!   writes ciphertext to a temp file and `rename`s it over the plaintext file, and
//!   the cold wrap `insert`s over the bare redb value; in both cases the OLD
//!   plaintext data blocks are merely *freed*, not zeroed (redb is copy-on-write and
//!   is not compacted here). Enable protects against the "logical FS read / stolen
//!   disk image" threat model, but it is NOT a plaintext-shredding operation:
//!   forensic recovery of freed blocks can still resurrect pre-enable plaintext. A
//!   compaction/vacuum step is a follow-up for callers that require shredding.
//! * **Coarse AEAD associated-data binding (inherited formats).** The `AEIX` format
//!   binds only its 10-byte header as AAD; the `ACV1` cold wrapper uses empty AAD
//!   (the `key_version` in the wrapper is not authenticated). Neither binds the
//!   ciphertext to its file identity / redb key / entity id, so wrapped blobs are
//!   *relocatable* by an attacker with write access (confidentiality and per-blob
//!   integrity still hold; only cross-entity placement is unauthenticated). This is
//!   a property of the reused #481/#3617 formats, not introduced here; binding file
//!   id / redb key (and the `key_version`) into the AAD is a defense-in-depth
//!   follow-up.
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
    ENABLE_KEY_VERSION, build_enable_cold_cipher, build_enable_wal_keyring, clear_rotation_state,
    mark_cold_complete, mark_index_complete, mark_wal_complete, mark_wal_retire_complete,
    read_enable_ledger, retire_enable_plaintext_wal, wrap_enable_index_files, write_enable_ledger,
};
use crate::encryption::config::KeyProviderConfig;
use crate::encryption::factory::Algorithm;

/// Outcome of a completed plaintext → encrypted enable migration.
///
/// Each flag reports whether that at-rest layer's migration pass ran (the layer
/// was in scope). `wal_migrated`, `index_migrated`, and `checkpoint_migrated` are
/// always `true` for a durable database (index/checkpoint ride one pass);
/// `cold_migrated` is `true` only when a cold tier was present. The key-source
/// reference recorded into the durable
/// [`encryption.state`](crate::db::encryption_state) authority is echoed so the
/// operator can confirm what the next `open()` will consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableReport {
    /// WAL segments rolled to the encrypted (v16) format.
    pub wal_migrated: bool,
    /// Plaintext index files wrapped into the `AEIX` format.
    pub index_migrated: bool,
    /// Checkpoint files covered (ride the index DEK/format wrap pass).
    pub checkpoint_migrated: bool,
    /// Cold-storage bare values wrapped into the `ACV1` format (only when a cold
    /// tier is present; `false` otherwise).
    pub cold_migrated: bool,
    /// The non-secret key-source reference recorded into the authority
    /// (File path / Env var name — never key bytes).
    pub key_source: KeyProviderConfig,
}

/// Map the WAL seam's **double-install** rejection to a `FailedPrecondition` —
/// enabling an already-encrypted WAL is a caller precondition failure, not an
/// internal fault (honors the PR2 `install_wal_keyring` TODO).
///
/// Only the distinguishable [`StorageError::WalKeyringAlreadyInstalled`] variant
/// is reclassified. A genuine WAL I/O / seal-and-reopen fault (a plain
/// [`StorageError::WalError`], or anything else) is a real internal failure and
/// is passed through UNCHANGED so it keeps its `INTERNAL` classification — it must
/// never be mislabeled as the non-retriable "already encrypted" precondition.
fn map_wal_install_err(e: Error) -> Error {
    match &e {
        Error::Storage(StorageError::WalKeyringAlreadyInstalled { reason }) => {
            Error::FailedPrecondition(format!(
                "cannot enable encryption: the WAL is already encrypted ({reason})"
            ))
        }
        _ => e,
    }
}

impl AletheiaDB {
    /// The AEAD algorithm an enable migration writes under (Issue #3616 PR3).
    ///
    /// Sourced from `config.encryption.algorithm` (retained on the DB as
    /// [`configured_encryption_algorithm`](AletheiaDB::configured_encryption_algorithm)
    /// even when encryption is disabled), NOT from the absent `encryption_config`
    /// of a plaintext database. This is the single source of truth for BOTH the
    /// enable write and the interrupted-enable resume: the resume's wrap passes and
    /// the ciphers `open()` builds from `config.encryption.algorithm`
    /// (`enable_resume_ciphers`) then read the identical value, so they can never
    /// diverge (the pre-fix bug: enable/resume used `Algorithm::default()` while the
    /// ciphers used the operator's concrete config algorithm → an undecryptable DB).
    ///
    /// The concrete (resolved) form is what gets PINNED into the authority at enable
    /// time, so a reopen — even on a different CPU class — rebuilds the same cipher.
    fn enable_algorithm(&self) -> Algorithm {
        self.configured_encryption_algorithm
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
    /// ## The quiesced window — NO persistence is permitted through this handle
    ///
    /// Between this call returning and the reopen, this handle must perform **no
    /// index/checkpoint/cold persistence at all** — neither the background thread
    /// (stopped + joined as step 1, not restarted) nor an **explicit
    /// [`persist_indexes`](AletheiaDB::persist_indexes) call**. The reason is
    /// structural: the live WAL is now encrypted, but this handle's index manager
    /// still carries a **plaintext** keyring (there is no live `None → Some`
    /// index-keyring install). A persist would therefore write **plaintext** index
    /// files over the freshly-wrapped `AEIX` snapshot — the exact corruption this
    /// engine prevents. To make that impossible rather than merely discouraged,
    /// [`persist_indexes`](AletheiaDB::persist_indexes) is **fail-closed** on a
    /// post-enable handle (it returns a `FailedPrecondition` telling you to reopen)
    /// so no code path can silently corrupt the encrypted snapshot.
    ///
    /// Continuing to WRITE through the returned handle is likewise **not
    /// supported** in v1 (index mutations would not be persisted by this handle;
    /// they survive only via the encrypted WAL until reopen). **You MUST reopen the
    /// database** (drop this handle and call [`AletheiaDB::open`]) to get a
    /// fully-operational encrypted instance: the next `open()` reads the flipped
    /// [`encryption.state`](crate::db::encryption_state) authority, brings every
    /// layer up under the cipher, and restarts the background persistence thread.
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
    /// * any layer migration (WAL / index / checkpoint / cold) or the durable
    ///   authority flip fails.
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
        if read_encryption_state(manager.base_path())?
            .map(|state| state.enabled)
            .unwrap_or(false)
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

        // Is a cold tier in scope? Its bare values are wrapped to `ACV1` below.
        let cold_in_scope = self.historical.read().has_tiered_storage();

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

        // === Step 1b: synchronous full persist while STILL PLAINTEXT =================
        // Capture ALL pre-enable state (current + historical, as of the current WAL
        // frontier) into the durable index snapshot NOW, before the WAL is sealed.
        // The index wrap below turns this snapshot into `AEIX`, so afterwards the
        // ENCRYPTED snapshot holds every pre-enable record — which is what makes it
        // safe to RETIRE the pre-enable plaintext WAL segments (Step 2c) without any
        // data loss (reopen reconstructs from the encrypted snapshot, not the deleted
        // plaintext WAL). Runs while the WAL is still plaintext, so the fail-closed
        // `persist_indexes` guard (post-enable handle) does not apply. This is a
        // superset of the worker's shutdown final-persist above — belt-and-suspenders.
        self.persist_indexes()?;

        // === Step 2: durable ledger (breadcrumb #1) then migrate every layer =========
        // WAL is always Pending; index + checkpoint are Pending (a durable database
        // always has an index dir to wrap); cold is Pending iff a cold tier exists.
        // The quiesce above ran the worker's shutdown final-persist while the DB was
        // still plaintext, so the index dir now holds a fresh PLAINTEXT snapshot the
        // wrap pass converts.
        write_enable_ledger(&manager, &key_source, true, cold_in_scope)?;

        // WAL: install the keyring (seal plaintext v13 → store → reopen encrypted
        // v16) via the PR2 seam, then record completion (breadcrumb #2).
        self.wal
            .install_wal_keyring(keyring)
            .map_err(map_wal_install_err)?;
        mark_wal_complete(&manager)?;

        // Index + checkpoint: wrap every bare plaintext index file into `AEIX` under
        // the index DEK, then record completion (breadcrumb #3).
        wrap_enable_index_files(&manager, &key_source, algorithm)?;
        mark_index_complete(&manager)?;

        // WAL plaintext retire: now that the ENCRYPTED (`AEIX`) index snapshot durably
        // holds every pre-enable record (Step 1b persist → this wrap), retire the
        // sealed pre-enable PLAINTEXT (v13) WAL segments so no cleartext survives at
        // rest (breadcrumb #3b). Idempotent + resumable: a crash mid-retire leaves
        // `wal_retire=Pending` and the resume re-runs it. The fresh encrypted (v16)
        // active segment is kept; reopen reconstructs pre-enable state from the
        // encrypted snapshot. Only in scope when the index snapshot exists
        // (`enable_scope` sets `wal_retire=Pending` iff index is in scope).
        retire_enable_plaintext_wal(&self.wal)?;
        mark_wal_retire_complete(&manager)?;

        // Cold: wrap every bare stored value into `ACV1` under the cold DEK on the
        // live (plaintext-keyring) cold store, then record completion (breadcrumb
        // #4). Byte-preserving; full encrypted cold reads resume at the next open()
        // (the live store's keyring stays `None` — see the reopen contract).
        if cold_in_scope {
            let tiered = self.historical.read().tiered_storage_arc().ok_or_else(|| {
                Error::FailedPrecondition(
                    "cold tier vanished between precondition check and migration".to_string(),
                )
            })?;
            let cold_cipher = build_enable_cold_cipher(&key_source, algorithm)?;
            tiered
                .cold_storage()
                .wrap_plaintext_cold_values(&cold_cipher, ENABLE_KEY_VERSION)?;
            mark_cold_complete(&manager)?;
        }

        // === Step 3: flip the authority BEFORE clearing the ledger (binding order) ===
        // (breadcrumb #5) A crash after this but before the clear resumes via
        // `resume_pending_enable`.
        // Pin the RESOLVED concrete algorithm (the same one every wrap pass above
        // wrote under) so the reopen rebuilds the identical cipher.
        write_encryption_state_durable(
            manager.base_path(),
            &EncryptionState::enabled_with_algorithm(key_source.clone(), algorithm),
        )?;

        // === Step 4: clear the ledger (breadcrumb #6) ================================
        clear_rotation_state(&manager);

        Ok(EnableReport {
            wal_migrated: true,
            index_migrated: true,
            checkpoint_migrated: true,
            cold_migrated: cold_in_scope,
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

        // Index + checkpoint: complete the plaintext -> `AEIX` wrap pass INLINE,
        // BEFORE the caller reads the index directory back (`open()` runs this
        // before `load_indexes`). The pass is idempotent — a crash mid-pass left a
        // mix of plaintext + `AEIX` files, and re-running wraps only the remaining
        // plaintext ones. The manager was built under the enable index DEK on this
        // startup (see `enable_resume_ciphers`), so both the wrap writes and the
        // subsequent index load read the encrypted bytes correctly.
        if view.index_pending || view.checkpoint_pending {
            wrap_enable_index_files(manager, &view.new_source, self.enable_algorithm())?;
            mark_index_complete(manager)?;
        }

        // WAL plaintext retire (resume): retire the pre-enable PLAINTEXT (v13) WAL
        // segments once the ENCRYPTED (`AEIX`) index snapshot holds the pre-enable
        // state (index Complete). Lossless on resume: (a) the original enable ran a
        // synchronous persist BEFORE writing this ledger, so the encrypted snapshot
        // durably holds every pre-enable record, and (b) startup already read every
        // WAL segment into memory (`startup_wal_entries`, before this runs), so the
        // subsequent differential replay re-covers them regardless — deleting the
        // segment FILES here cannot lose an in-memory entry. Gated on `index_complete`
        // so a (synthetic) no-index-snapshot enable never deletes plaintext WAL.
        // Idempotent: re-runs after a crash mid-retire. Re-read to observe the index
        // completion just recorded.
        if let Some(mid) = read_enable_ledger(manager)?
            && mid.wal_retire_pending
            && mid.index_complete
        {
            retire_enable_plaintext_wal(&self.wal)?;
            mark_wal_retire_complete(manager)?;
        }

        // Re-read the ledger to reflect the index/checkpoint completions just
        // recorded, then decide on the flip from the FRESH per-field state (never a
        // stale in-memory view). If any non-WAL layer is still `Pending` it can only
        // be cold: the cold store is not wired onto `self.historical` at this point
        // in `open()` (it is constructed AFTER index load), so DEFER the cold wrap +
        // the authority flip + the ledger clear to `resume_pending_enable_cold`,
        // which runs once the cold store exists. Leaving the ledger present is the
        // binding-order guarantee: the authority is not flipped until every layer,
        // cold included, is settled.
        let Some(fresh) = read_enable_ledger(manager)? else {
            return Ok(());
        };
        if !fresh.non_wal_layers_settled() {
            return Ok(());
        }

        // Every non-WAL layer is settled and there is no cold work to defer: flip
        // the authority BEFORE clearing the ledger (idempotent if already flipped —
        // the flip->clear-gap resume case).
        write_encryption_state_durable(
            manager.base_path(),
            &EncryptionState::enabled_with_algorithm(
                fresh.new_source.clone(),
                self.enable_algorithm(),
            ),
        )?;
        clear_rotation_state(manager);
        Ok(())
    }

    /// Finish an interrupted enable migration's COLD layer at `open()` time, once
    /// the cold store has been wired onto `self.historical` (Issue #3616 PR3).
    ///
    /// [`resume_pending_enable`] runs before the cold tier is constructed, so it
    /// cannot wrap cold values; it therefore DEFERS the cold wrap, the authority
    /// flip, and the ledger clear to this method, which `open()` calls after the
    /// cold store is set. This preserves the binding order — the authority is
    /// flipped only after **every** layer (cold included) is settled.
    ///
    /// The cold store is built under the enable cold DEK on this startup (see
    /// [`enable_resume_ciphers`](crate::db::rotation::enable_resume_ciphers)), so
    /// the wrap-only pass rewrites each bare value to `ACV1` and the resumed
    /// session reads its own wrapped values. Idempotent / resumable: a value
    /// already wrapped at the enable version is skipped, and the pass advances a
    /// durable redb cursor so a crash mid-pass resumes with no double-encrypt.
    ///
    /// **Guarded no-op:** returns `Ok(())` when no pending-enable ledger exists or
    /// its cold layer is not `Pending`.
    pub(crate) fn resume_pending_enable_cold(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_enable_ledger(manager)? else {
            return Ok(());
        };
        if !view.cold_pending {
            return Ok(());
        }

        // Cold is Pending but the store is not wired: the ledger claims a cold tier
        // that this open() did not construct. Fail closed rather than flip the
        // authority while cold values remain bare under it.
        let tiered = self.historical.read().tiered_storage_arc().ok_or_else(|| {
            Error::FailedPrecondition(
                "pending enable ledger records a cold layer, but no cold tier is configured on \
                 this open(); reopen with the cold tier enabled so the migration can complete"
                    .to_string(),
            )
        })?;
        let cold_cipher = build_enable_cold_cipher(&view.new_source, self.enable_algorithm())?;
        tiered
            .cold_storage()
            .wrap_plaintext_cold_values(&cold_cipher, ENABLE_KEY_VERSION)?;
        mark_cold_complete(manager)?;

        // Every layer is now settled: binding order — flip the authority BEFORE
        // clearing the ledger (idempotent if already flipped).
        write_encryption_state_durable(
            manager.base_path(),
            &EncryptionState::enabled_with_algorithm(
                view.new_source.clone(),
                self.enable_algorithm(),
            ),
        )?;
        clear_rotation_state(manager);
        Ok(())
    }

    /// Fail LOUDLY at startup when an interrupted enable recorded a cold layer as
    /// `Pending` but this `open()` will NOT construct a cold tier (Issue #3616 PR3).
    ///
    /// [`resume_pending_enable_cold`](Self::resume_pending_enable_cold)'s own
    /// fail-closed guard only runs when the cold-init block is entered. If the
    /// operator reopens with the cold tier disabled or dropped (a trimmed config, a
    /// moved cold path), that block is skipped and the guard is **unreachable**, so
    /// the enable ledger would otherwise wedge forever — `resume_pending_enable`
    /// defers (cold still `Pending` → `non_wal_layers_settled() == false`), the
    /// authority is never flipped, and nothing tells the operator the enable is
    /// stuck. This converts that silent limbo into the same actionable
    /// `FailedPrecondition` the in-block guard emits. Called on the startup branch
    /// that does NOT build a cold tier. A guarded no-op when no pending-enable
    /// ledger exists or its cold layer is not `Pending`.
    pub(crate) fn fail_if_pending_enable_cold_without_tier(&self) -> Result<()> {
        let Some(manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };
        let Some(view) = read_enable_ledger(manager)? else {
            return Ok(());
        };
        if view.cold_pending {
            return Err(Error::FailedPrecondition(
                "pending enable ledger records a cold layer, but no cold tier is configured on \
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

    /// Like [`plaintext_durable_config`] but with a CONCRETE (non-`Auto`)
    /// `encryption.algorithm` while `enabled=false` — the natural "pre-configure the
    /// algorithm, then turn it on" operator workflow (Issue #3616 PR3 algorithm pin).
    fn plaintext_durable_config_with_algorithm(
        data_dir: &Path,
        algorithm: crate::encryption::factory::Algorithm,
    ) -> AletheiaDBConfig {
        let mut cfg = plaintext_durable_config(data_dir);
        cfg.encryption = crate::encryption::config::EncryptionConfig {
            enabled: false,
            algorithm,
            ..Default::default()
        };
        cfg
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

    /// Like [`plaintext_durable_config`] but WITH a plaintext cold (redb) tier, so
    /// the enable cold bare→ACV1 wrap pass is exercised.
    fn plaintext_durable_config_with_cold(data_dir: &Path) -> AletheiaDBConfig {
        use crate::config::HistoricalConfigBuilder;
        use std::time::Duration;
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
            .build()
    }

    /// The `indexes/` dir under the manager base path (where the actual index
    /// files live; the control-plane `rotation.state`/`encryption.state` sit one
    /// level up in the base dir).
    fn index_files_dir(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("indexes").join("indexes")
    }

    /// Count the index files under `dir`, returning (total, plaintext_count,
    /// aeix_count) — walking recursively and skipping atomic-write scratch files.
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

    /// Seed a bare (plaintext, unwrapped) node version directly into the cold
    /// store of `db`, returning its version-id `u64`.
    fn seed_bare_cold_node(db: &AletheiaDB, vid_u64: u64, node_id: u64, name: &str) -> u64 {
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
        assert!(!cold.is_encrypted(), "seed store is plaintext");
        cold.store_node_version(&node).unwrap();
        assert!(
            !cold.raw_node_value_is_acv1_for_test(vid_u64),
            "seeded cold value must be bare (not ACV1) before enable"
        );
        vid_u64
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
            assert!(report.index_migrated && report.checkpoint_migrated);
            assert!(!report.cold_migrated, "no cold tier in this config");
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
        drop(db);

        // Whole-directory sweep: complementing the per-file round-trip magic asserts
        // elsewhere, sweep the ENTIRE index data dir and assert NOT ONE persisted
        // index file (bitcode manifest/interner/graph/temporal/temporal_adjacency/
        // vector-meta + native usearch files) still begins with plaintext bytes —
        // every one carries the `AEIX` header. This is the guard against a future
        // NEW persist path that uses the plaintext writer instead of the cipher-aware
        // one: such a path compiles fine and silently writes PLAINTEXT, and only a
        // full-dir scan (not a targeted round-trip on the files we happen to know
        // about) catches it.
        let idx_dir = index_files_dir(dir.path());
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(
            total > 0,
            "enable + reopen persisted an encrypted index snapshot to sweep"
        );
        assert_eq!(
            plain, 0,
            "no plaintext index file survives enable (whole-dir sweep)"
        );
        assert_eq!(
            aeix, total,
            "every persisted index file is AEIX after enable"
        );
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

    /// A1: `map_wal_install_err` reclassifies ONLY the distinguishable
    /// double-install rejection to `FailedPrecondition`; a genuine WAL I/O fault
    /// (a plain `WalError`) is passed through unchanged and is NEVER mislabeled as
    /// the non-retriable "already encrypted" precondition.
    #[test]
    fn map_wal_install_err_distinguishes_double_install_from_io_fault() {
        use crate::core::error::{Error, StorageError};

        // The seam's double-install rejection → FailedPrecondition ("already encrypted").
        let double_install = Error::Storage(StorageError::WalKeyringAlreadyInstalled {
            reason: "a WAL keyring is already installed".to_string(),
        });
        let mapped = super::map_wal_install_err(double_install);
        match mapped {
            Error::FailedPrecondition(msg) => {
                assert!(
                    msg.contains("already encrypted"),
                    "double-install maps to the already-encrypted precondition, got: {msg}"
                );
            }
            other => panic!("expected FailedPrecondition, got: {other:?}"),
        }

        // A genuine WAL I/O / seal fault must fall through UNCHANGED (INTERNAL-class),
        // never becoming a false "already encrypted" precondition.
        let io_fault = Error::Storage(StorageError::WalError {
            reason: "failed to fsync sealed segment: disk full".to_string(),
        });
        let mapped = super::map_wal_install_err(io_fault);
        match mapped {
            Error::Storage(StorageError::WalError { reason }) => {
                assert!(reason.contains("disk full"), "genuine I/O fault preserved");
            }
            other => panic!("I/O WalError must not be reclassified, got: {other:?}"),
        }
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

    /// C1: crash AFTER the ledger but BEFORE the WAL roll — the WAL is still
    /// plaintext AND the index dir is still plaintext. This models the REAL ledger
    /// shape a durable enable writes: `wal=Pending` **and** `index=Pending`
    /// simultaneously (a durable enable always records `index_in_scope=true`, config
    /// line 242). Reopen must resume BOTH layers: install+roll the WAL, wrap the
    /// still-plaintext index dir to `AEIX`, and only THEN flip the authority + clear
    /// the ledger — never flipping over a plaintext index under an "encrypted"
    /// authority.
    #[test]
    fn enable_crash_after_ledger_before_wal_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().build())
                .unwrap();
            // Force a plaintext index snapshot to disk so the index dir is a genuine
            // plaintext layer the resume must wrap.
            db.persist_indexes().unwrap();
        }
        // Precondition: index dir is plaintext (no AEIX yet), WAL untouched.
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(total > 0, "expected a persisted plaintext index snapshot");
        assert_eq!(aeix, 0, "index starts fully plaintext");
        assert_eq!(plain, total);

        // Simulate the interrupted state with the REAL shape: wal=Pending AND
        // index=Pending (index_in_scope=true), WAL + index bytes still plaintext.
        write_enable_ledger(&manager_for(dir.path()), &key, true, false).unwrap();
        assert!(indexes.join("rotation.state").exists());

        // Reopen: resume completes BOTH the WAL roll and the index wrap.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1);
            assert!(db.wal.is_encrypted(), "resume rolled the WAL to encrypted");
        }
        // The still-plaintext index dir was wrapped to AEIX by the combined-pending
        // resume (authority is only flipped AFTER the index is fully wrapped).
        let (total2, plain2, aeix2) = classify_index_files(&idx_dir);
        assert_eq!(plain2, 0, "resume wrapped every plaintext index file");
        assert!(
            aeix2 > 0 && aeix2 == total2,
            "index dir is fully AEIX after resume"
        );
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped only after index wrapped");
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
            // Real durable shape: index_in_scope=true (index=Pending); the index is
            // already AEIX on disk from block-1, so the resume wrap is an idempotent
            // skip, but the ledger shape matches what a durable enable actually writes.
            write_enable_ledger(&mgr, &key, true, false).unwrap();
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
        // Re-lay a settled (wal=Complete, index=Pending) ledger matching the real
        // durable shape (index_in_scope=true), leaving the authority ENABLED.
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, true, false).unwrap();
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

    /// Index plaintext→AEIX round-trip: persist plaintext index files, enable, and
    /// assert every on-disk index file is `AEIX` (not plaintext); reopen and read
    /// the data back identically. Encrypted bytes are never mis-read as plaintext.
    #[test]
    fn enable_index_plaintext_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let idx_dir = index_files_dir(dir.path());

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", "alice").build(),
            )
            .unwrap();
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", "bob").build(),
            )
            .unwrap();
            // Force a plaintext index snapshot to disk.
            db.persist_indexes().unwrap();
            let (total, plain, aeix) = classify_index_files(&idx_dir);
            assert!(total > 0, "expected persisted index files on disk");
            assert_eq!(aeix, 0, "index files start plaintext");
            assert_eq!(plain, total);

            let report = db.enable_encryption(key.clone()).unwrap();
            assert!(report.index_migrated && report.checkpoint_migrated);

            // Every index file is now AEIX; none left plaintext.
            let (total2, plain2, aeix2) = classify_index_files(&idx_dir);
            assert_eq!(total2, total, "no file added/lost by the wrap pass");
            assert_eq!(plain2, 0, "no plaintext index file survives the wrap");
            assert_eq!(aeix2, total, "every index file is AEIX after enable");
        }

        // Reopen under the flipped authority: the AEIX index snapshot decrypts, the
        // data survives, and the files on disk are still AEIX (not re-plaintexted).
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 2, "nodes survive reopen via AEIX index");
            assert!(db.wal.is_encrypted());
        }
        let (_, plain3, aeix3) = classify_index_files(&idx_dir);
        assert!(
            aeix3 > 0 && plain3 == 0,
            "index dir remains AEIX after reopen"
        );
    }

    /// AC(i) NEGATIVE reopen-contract test: an explicit `persist_indexes()` on the
    /// QUIESCED post-enable handle is fail-closed and does NOT corrupt the
    /// encrypted (`AEIX`) snapshot with plaintext. Proves the documented v1
    /// limitation is enforced, not merely described.
    #[test]
    fn persist_on_quiesced_post_enable_handle_is_refused_and_preserves_aeix() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let idx_dir = index_files_dir(dir.path());

        let mut db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("n", "alice").build(),
        )
        .unwrap();
        db.persist_indexes().unwrap();

        db.enable_encryption(key.clone()).unwrap();
        // Post-enable: every index file is AEIX.
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert!(total > 0);
        assert_eq!(plain, 0, "index files are AEIX after enable");
        assert_eq!(aeix, total);

        // The fail-closed guard: an explicit persist through the quiesced handle is
        // REFUSED rather than silently writing plaintext over the AEIX snapshot.
        let err = db.persist_indexes().unwrap_err();
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "persist on a post-enable quiesced handle must be fail-closed, got: {err:?}"
        );

        // The snapshot is untouched: still every-file-AEIX, zero plaintext.
        let (total2, plain2, aeix2) = classify_index_files(&idx_dir);
        assert_eq!(
            total2, total,
            "no file added/removed by the refused persist"
        );
        assert_eq!(
            plain2, 0,
            "no plaintext index file was written over the AEIX snapshot"
        );
        assert_eq!(aeix2, total);

        drop(db);
        // Reopen: data intact, snapshot still AEIX (never corrupted).
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        assert_eq!(
            db.node_count(),
            1,
            "data survives; snapshot was not corrupted"
        );
        assert!(db.wal.is_encrypted());
        let (_, plain3, aeix3) = classify_index_files(&idx_dir);
        assert!(
            aeix3 > 0 && plain3 == 0,
            "index dir remains AEIX after reopen"
        );
    }

    /// Checkpoint files ride the index DEK/format, so an index snapshot (the
    /// durable manifest + index files — the checkpoint of current state) written
    /// plaintext is wrapped by the same pass and remains readable after
    /// enable+reopen. Proven by loading from the snapshot alone (WAL truncated to
    /// nothing to replay would still yield the data from the encrypted snapshot).
    #[test]
    fn enable_checkpoint_plaintext_readable_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let idx_dir = index_files_dir(dir.path());
        let manifest = idx_dir.join("manifest.idx");

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("Doc", PropertyMapBuilder::new().insert("t", "spec").build())
                .unwrap();
            db.persist_indexes().unwrap();
            assert!(manifest.exists(), "manifest (index checkpoint) written");
            let bytes = std::fs::read(&manifest).unwrap();
            assert!(
                !crate::storage::index_persistence::common::is_encrypted_index(&bytes),
                "manifest starts plaintext"
            );

            db.enable_encryption(key.clone()).unwrap();

            let bytes = std::fs::read(&manifest).unwrap();
            assert!(
                crate::storage::index_persistence::common::is_encrypted_index(&bytes),
                "manifest (checkpoint) is AEIX-wrapped after enable"
            );
        }

        // Reopen: the encrypted manifest + index snapshot load; the node is present.
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        assert_eq!(db.node_count(), 1, "checkpoint survives enable + reopen");
    }

    /// IDX mid-pass crash: a crash DURING the index wrap pass leaves a MIX of
    /// plaintext + AEIX files plus a `wal=Complete, index=Pending` ledger (authority
    /// off). Reopen must resume — wrap the remaining plaintext files, flip the
    /// authority, clear the ledger — and never mis-read the mixed dir as plaintext.
    #[test]
    fn enable_crash_mid_index_wrap_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("k", "v").build())
                .unwrap();
            db.persist_indexes().unwrap();
            // Fully enable (WAL encrypted, index AEIX, authority flipped, cleared).
            db.enable_encryption(key.clone()).unwrap();
        }

        // Rewind to "mid index wrap": authority removed, a `wal=Complete,
        // index=Pending` enable ledger re-laid, and a subset of index files rolled
        // back to plaintext to simulate an interrupted pass. The WAL stays
        // encrypted on disk (the WAL layer already completed).
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, true, false).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
        }
        // Un-wrap ONE AEIX file back to its plaintext body to fake a partial pass.
        {
            use crate::storage::index_persistence::common::{
                decrypt_index_bytes, is_encrypted_index,
            };
            let algorithm = crate::encryption::factory::Algorithm::default();
            let cipher = crate::db::rotation::build_enable_index_cipher(&key, algorithm).unwrap();
            // Find one AEIX file and rewrite it as its decrypted plaintext body.
            let mut un_wrapped = false;
            fn first_file(dir: &Path, out: &mut Option<std::path::PathBuf>) {
                for e in std::fs::read_dir(dir).unwrap() {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        first_file(&p, out);
                    } else if out.is_none() {
                        *out = Some(p);
                    }
                }
            }
            let mut candidate = None;
            first_file(&idx_dir, &mut candidate);
            if let Some(p) = candidate {
                let bytes = std::fs::read(&p).unwrap();
                if is_encrypted_index(&bytes) {
                    let plain = decrypt_index_bytes(&bytes, &p, Some(&cipher)).unwrap();
                    std::fs::write(&p, &plain).unwrap();
                    un_wrapped = true;
                }
            }
            assert!(un_wrapped, "test must roll back at least one AEIX file");
        }
        let (_, plain_before, aeix_before) = classify_index_files(&idx_dir);
        assert!(
            plain_before > 0 && aeix_before > 0,
            "precondition: a genuine mix of plaintext + AEIX files"
        );

        // Reopen: resume wraps the remaining plaintext, reads the mix correctly.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1, "data intact after mid-pass resume");
            assert!(db.wal.is_encrypted());
        }
        let (_, plain_after, aeix_after) = classify_index_files(&idx_dir);
        assert_eq!(
            plain_after, 0,
            "resume wrapped every remaining plaintext file"
        );
        assert!(aeix_after > 0);
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped by resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// Cold bare→ACV1 round-trip: seed a cold store with BARE values, enable,
    /// reopen, read back identical, and assert the on-disk values are ACV1-wrapped
    /// (a bare value is never mis-read as legacy ciphertext once wrapped).
    #[test]
    fn enable_cold_bare_plaintext_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let cold_vid = 9001u64;

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_bare_cold_node(&db, cold_vid, 500, "Carol");
            db.persist_indexes().unwrap();

            let report = db.enable_encryption(key.clone()).unwrap();
            assert!(report.cold_migrated, "cold tier in scope → migrated");

            // On-disk cold value is now ACV1-wrapped.
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            assert!(
                tiered
                    .cold_storage()
                    .raw_node_value_is_acv1_for_test(cold_vid),
                "cold value wrapped to ACV1 after enable"
            );
        }

        // Reopen under the flipped authority: the cold store is built encrypted,
        // the ACV1 value decrypts back to the identical record, hot data survives.
        {
            let db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            assert!(db.wal.is_encrypted(), "reopen honors the durable authority");
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            let cold = tiered.cold_storage();
            assert!(cold.is_encrypted(), "cold tier built encrypted on reopen");
            assert!(
                cold.raw_node_value_is_acv1_for_test(cold_vid),
                "cold value stays ACV1 across reopen"
            );
            let loaded = cold
                .get_node_version(crate::core::id::VersionId::new(cold_vid).unwrap())
                .unwrap()
                .expect("ACV1 cold value decrypts on reopen");
            use crate::core::version::EntityVersion;
            assert_eq!(loaded.version_id().as_u64(), cold_vid);
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled);
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// COLD mid-pass crash: a crash DURING the cold wrap pass leaves some values
    /// ACV1-wrapped and some bare, plus a `wal/index=Complete, cold=Pending` enable
    /// ledger (authority off). Reopen must resume the wrap from the durable cursor,
    /// flip the authority, and clear the ledger — never mis-reading a bare or a
    /// wrapped value.
    #[test]
    fn enable_crash_mid_cold_wrap_resumes() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let cold_path = dir.path().join("cold.redb");
        let (vid_a, vid_b) = (9101u64, 9102u64);
        let algorithm = crate::encryption::factory::Algorithm::default();

        // Block 1: a FULL enable over a cold tier (WAL encrypted, index AEIX, both
        // cold values ACV1, authority flipped, ledger cleared). This gives a
        // consistent fully-migrated on-disk state we then rewind.
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_bare_cold_node(&db, vid_a, 501, "Alice");
            seed_bare_cold_node(&db, vid_b, 502, "Bob");
            db.persist_indexes().unwrap();
            db.enable_encryption(key.clone()).unwrap();
        }

        // Rewind to "mid cold wrap": remove the authority, re-lay a
        // `wal/index=Complete, cold=Pending` ledger, and roll cold value B back to
        // BARE (A stays ACV1) so the store is a genuine partial mix. All done with
        // NO live db (no racing background thread): read B under the enable cold
        // DEK, then rewrite it bare through a plaintext-keyring store.
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, true, true).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
            crate::db::rotation::mark_index_complete(&mgr).unwrap();
        }
        {
            use crate::core::version::EntityVersion;
            let cold_cipher =
                crate::db::rotation::build_enable_cold_cipher(&key, algorithm).unwrap();
            // Read B under the cipher (it is ACV1 on disk), then drop the handle.
            let node_b = {
                let cold = RedbColdStorage::new(&cold_path, RedbConfig::new())
                    .unwrap()
                    .with_cipher(cold_cipher);
                assert!(cold.raw_node_value_is_acv1_for_test(vid_a));
                assert!(cold.raw_node_value_is_acv1_for_test(vid_b));
                cold.get_node_version(crate::core::id::VersionId::new(vid_b).unwrap())
                    .unwrap()
                    .unwrap()
            };
            assert_eq!(node_b.version_id().as_u64(), vid_b);
            // Rewrite B bare via a plaintext-keyring store (single handle at a time).
            let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
            cold.delete_node_version(crate::core::id::VersionId::new(vid_b).unwrap())
                .unwrap();
            cold.store_node_version(&node_b).unwrap();
            assert!(
                cold.raw_node_value_is_acv1_for_test(vid_a),
                "A stays ACV1 (already wrapped)"
            );
            assert!(
                !cold.raw_node_value_is_acv1_for_test(vid_b),
                "B rolled back to bare to model a mid-cold-pass crash"
            );
        }

        // Reopen: resume wraps the remaining bare cold value, flips, clears.
        {
            let db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            assert!(db.wal.is_encrypted());
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            let cold = tiered.cold_storage();
            assert!(
                cold.raw_node_value_is_acv1_for_test(vid_a)
                    && cold.raw_node_value_is_acv1_for_test(vid_b),
                "both cold values are ACV1 after mid-pass resume"
            );
            // Both decrypt back.
            use crate::core::version::EntityVersion;
            for vid in [vid_a, vid_b] {
                let loaded = cold
                    .get_node_version(crate::core::id::VersionId::new(vid).unwrap())
                    .unwrap()
                    .expect("cold value decrypts after resume");
                assert_eq!(loaded.version_id().as_u64(), vid);
            }
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped by cold resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// C1 (data-at-rest): after enable on a NO-COLD database, NO pre-enable
    /// PLAINTEXT (v13) WAL segment survives on disk, AND all pre-enable data still
    /// reads back after reopen — proving the engine's synchronous persist captured
    /// every record into the ENCRYPTED snapshot BEFORE the plaintext WAL was retired
    /// (reopen reconstructs from the encrypted snapshot, not the deleted WAL).
    /// Deliberately does NOT persist before enable, so the plaintext WAL is the only
    /// pre-enable copy until the engine's own persist runs.
    #[test]
    fn enable_retires_plaintext_wal_and_reopen_reads_from_encrypted_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());

        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", "alice").build(),
            )
            .unwrap();
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("n", "bob").build(),
            )
            .unwrap();
            assert!(!db.wal.is_encrypted(), "starts plaintext");

            db.enable_encryption(key.clone()).unwrap();
            assert!(db.wal.is_encrypted());
            // The pre-enable plaintext (legacy) WAL segments are retired: every
            // remaining segment is the encrypted (v16) enable generation.
            assert!(
                db.wal
                    .all_segments_use_key_version(crate::db::rotation::ENABLE_KEY_VERSION),
                "no pre-enable plaintext WAL segment remains after enable"
            );
        }

        // Reopen: the two nodes reconstruct from the ENCRYPTED snapshot — the
        // plaintext WAL that once held them is gone.
        let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
        assert_eq!(
            db.node_count(),
            2,
            "pre-enable data survives via the encrypted snapshot after plaintext WAL retire"
        );
        assert!(db.wal.is_encrypted());
        assert!(
            db.wal
                .all_segments_use_key_version(crate::db::rotation::ENABLE_KEY_VERSION),
            "still no plaintext WAL segment after reopen"
        );
    }

    /// C2 (crash mid-retire): a crash after the WAL roll + index wrap but BEFORE the
    /// pre-enable plaintext WAL is retired leaves a genuine sealed plaintext (v13)
    /// segment plus a `wal_retire=Pending` ledger (authority off). Reopen must resume
    /// — retire the remaining plaintext segment, flip the authority, clear the ledger
    /// — losslessly (the encrypted snapshot + the startup replay both hold the data).
    #[test]
    fn enable_crash_mid_wal_retire_resumes() {
        use crate::encryption::factory::Algorithm;
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let algorithm = Algorithm::default();

        // Drive the enable up to JUST BEFORE the plaintext-WAL retire, leaving a real
        // sealed plaintext segment on disk. The index wrap is done AFTER the db (and
        // its plaintext background worker) is dropped, so no worker can race a
        // plaintext persist over the AEIX files.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("k", "v").build())
                .unwrap();
            db.persist_indexes().unwrap();

            let mgr = manager_for(dir.path());
            // Ledger: wal=Pending, index=Pending, wal_retire=Pending (index in scope).
            write_enable_ledger(&mgr, &key, true, false).unwrap();
            // Roll the WAL to encrypted: seal the plaintext v13 segment, open v16.
            let keyring = crate::db::rotation::build_enable_wal_keyring(&key, algorithm).unwrap();
            db.wal.install_wal_keyring(keyring).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
            assert!(
                !db.wal
                    .all_segments_use_key_version(crate::db::rotation::ENABLE_KEY_VERSION),
                "a sealed plaintext segment still exists before retire"
            );
            // Drop the db (index still plaintext → the worker's final persist is a
            // safe plaintext write, consistent with index=Pending).
        }
        // Wrap the index to AEIX standalone (no live worker to race), mark it complete.
        crate::db::rotation::wrap_enable_index_files(&manager_for(dir.path()), &key, algorithm)
            .unwrap();
        crate::db::rotation::mark_index_complete(&manager_for(dir.path())).unwrap();
        // CRASH POINT: sealed plaintext v13 on disk, AEIX snapshot, ledger
        // {wal=Complete, index=Complete, wal_retire=Pending}, NO authority.

        // Reopen: resume retires the plaintext segment, flips the authority, clears.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path())).unwrap();
            assert_eq!(db.node_count(), 1, "data intact after mid-retire resume");
            assert!(db.wal.is_encrypted());
            assert!(
                db.wal
                    .all_segments_use_key_version(crate::db::rotation::ENABLE_KEY_VERSION),
                "resume retired the remaining plaintext WAL segment"
            );
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped by mid-retire resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// B1 (the HIGH pin bug): enabling under a CONCRETE algorithm that differs from
    /// what `Auto` resolves to on this host must round-trip through reopen. Before
    /// the fix, enable wrote under `Algorithm::default()` (Auto→AES-NI host→AES) while
    /// the reopen built ChaCha ciphers from the operator's TOML → AEAD failure → an
    /// UNOPENABLE DB. Now enable writes under the configured algorithm AND pins the
    /// resolved concrete form into the authority, so reopen uses the identical cipher
    /// — and the pin OVERRIDES a divergent TOML algorithm (portability across a TOML
    /// edit or a cross-CPU host).
    #[test]
    fn enable_under_concrete_chacha_roundtrips_through_reopen() {
        use crate::encryption::factory::Algorithm;
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        {
            let mut db = AletheiaDB::with_unified_config(plaintext_durable_config_with_algorithm(
                dir.path(),
                Algorithm::ChaCha20Poly1305,
            ))
            .unwrap();
            db.create_node("Person", PropertyMapBuilder::new().insert("n", "z").build())
                .unwrap();
            db.persist_indexes().unwrap();
            db.enable_encryption(key.clone()).unwrap();
        }

        // The authority pins the concrete ChaCha algorithm actually written under.
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled);
        assert_eq!(state.algorithm, Some(Algorithm::ChaCha20Poly1305));

        // Reopen under the SAME concrete-ChaCha config: data survives (the pre-fix
        // break would fail here on an AES-NI host, where the enable wrote AES).
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config_with_algorithm(
                dir.path(),
                Algorithm::ChaCha20Poly1305,
            ))
            .unwrap();
            assert_eq!(db.node_count(), 1, "ChaCha-enabled DB reopens and decrypts");
            assert!(db.wal.is_encrypted());
        }

        // Reopen under a DIVERGENT TOML algorithm (AES) AND with algorithm=Auto: the
        // PINNED ChaCha in the authority overrides both, so the DB still opens — proof
        // the pin makes reopen immune to TOML edits / cross-CPU Auto resolution.
        for divergent in [Algorithm::Aes256Gcm, Algorithm::Auto] {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config_with_algorithm(
                dir.path(),
                divergent,
            ))
            .unwrap();
            assert_eq!(
                db.node_count(),
                1,
                "pinned ChaCha overrides a divergent config algorithm ({divergent:?})"
            );
            assert!(db.wal.is_encrypted());
        }
    }

    /// B1 (resume intra-run consistency): an interrupted enable resumed under a
    /// CONCRETE config algorithm must write AND read every layer under that ONE
    /// algorithm. Before the fix the resume wrap passes used `Algorithm::default()`
    /// (Auto) while the ciphers `open()` built used the concrete config algorithm →
    /// the resume manufactured unreadable AEIX/ACV1 within a single run.
    #[test]
    fn enable_resume_under_concrete_algorithm_writes_and_reads_consistently() {
        use crate::encryption::factory::Algorithm;
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let idx_dir = index_files_dir(dir.path());

        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config_with_algorithm(
                dir.path(),
                Algorithm::ChaCha20Poly1305,
            ))
            .unwrap();
            db.create_node("N", PropertyMapBuilder::new().insert("k", "v").build())
                .unwrap();
            db.persist_indexes().unwrap();
        }
        // Interrupted enable (real shape: wal=Pending AND index=Pending), all bytes
        // still plaintext.
        write_enable_ledger(&manager_for(dir.path()), &key, true, false).unwrap();

        // Reopen under the concrete-ChaCha config: resume rolls the WAL, wraps the
        // index, flips + clears — all under ChaCha, all mutually readable.
        {
            let db = AletheiaDB::with_unified_config(plaintext_durable_config_with_algorithm(
                dir.path(),
                Algorithm::ChaCha20Poly1305,
            ))
            .unwrap();
            assert_eq!(
                db.node_count(),
                1,
                "resume-wrapped bytes decrypt under ChaCha"
            );
            assert!(db.wal.is_encrypted());
        }
        let (total, plain, aeix) = classify_index_files(&idx_dir);
        assert_eq!(plain, 0, "resume wrapped every index file under ChaCha");
        assert!(aeix == total && aeix > 0);
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled);
        assert_eq!(
            state.algorithm,
            Some(Algorithm::ChaCha20Poly1305),
            "resume pins ChaCha"
        );
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }

    /// A5: an interrupted enable with `cold=Pending`, reopened with the cold tier
    /// DISABLED in config, must FAIL LOUDLY (not silently wedge). The in-block
    /// `resume_pending_enable_cold` guard is unreachable when no cold tier is built,
    /// so the startup else-branch guard converts the silent limbo into an actionable
    /// `FailedPrecondition`.
    #[test]
    fn enable_cold_pending_reopened_without_cold_tier_fails_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");

        // Block 1: a FULL enable over a cold tier, giving a consistent migrated state.
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_bare_cold_node(&db, 9201u64, 601, "Dan");
            db.persist_indexes().unwrap();
            db.enable_encryption(key.clone()).unwrap();
        }

        // Rewind to "wal/index=Complete, cold=Pending": remove the authority and
        // re-lay a cold-in-scope ledger with only the cold layer still Pending.
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, true, true).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
            crate::db::rotation::mark_index_complete(&mgr).unwrap();
        }

        // Reopen WITHOUT the cold tier: the enable ledger's cold layer is Pending but
        // no cold tier will be built → open() must FAIL LOUDLY rather than wedge.
        let err = AletheiaDB::with_unified_config(plaintext_durable_config(dir.path()))
            .expect_err("reopen without the cold tier must fail loudly, not wedge silently");
        assert!(
            matches!(err, crate::core::error::Error::FailedPrecondition(_)),
            "cold-Pending without a cold tier is a FailedPrecondition, got: {err:?}"
        );

        // The ledger is still present (never cleared) and the authority never flipped
        // — the migration is recoverable by reopening WITH the cold tier.
        assert!(indexes.join("rotation.state").exists(), "ledger preserved");
        assert!(
            read_encryption_state(&indexes).unwrap().is_none(),
            "authority not flipped"
        );

        // Reopening WITH the cold tier completes the migration (proves recoverability).
        {
            let db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            assert!(db.wal.is_encrypted());
        }
        assert!(read_encryption_state(&indexes).unwrap().unwrap().enabled);
        assert!(
            !indexes.join("rotation.state").exists(),
            "ledger cleared after cold resume"
        );
    }

    /// A6/F3 crash-point: all layers `Complete` (including cold) but the authority
    /// still OFF (the window between `mark_cold_complete` and the authority flip).
    /// Reopen must flip + clear via `resume_pending_enable`'s phase-1 path (this
    /// happens BEFORE the cold store is even built, since every cold value is already
    /// ACV1 and `non_wal_layers_settled()` is true).
    #[test]
    fn enable_crash_all_layers_complete_cold_authority_off_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let key = key_source_at(dir.path());
        let indexes = dir.path().join("indexes");
        let cold_vid = 9301u64;

        // Block 1: a FULL enable over a cold tier (all layers migrated).
        {
            let mut db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            db.create_node("Hot", PropertyMapBuilder::new().insert("h", "1").build())
                .unwrap();
            seed_bare_cold_node(&db, cold_vid, 610, "Erin");
            db.persist_indexes().unwrap();
            db.enable_encryption(key.clone()).unwrap();
        }

        // Rewind to "all layers Complete, authority OFF": remove the authority and
        // re-lay a ledger with every in-scope layer marked Complete (cold values are
        // already ACV1 on disk from block 1).
        std::fs::remove_file(encryption_state_path(&indexes)).unwrap();
        {
            let mgr = manager_for(dir.path());
            write_enable_ledger(&mgr, &key, true, true).unwrap();
            crate::db::rotation::mark_wal_complete(&mgr).unwrap();
            crate::db::rotation::mark_index_complete(&mgr).unwrap();
            crate::db::rotation::mark_cold_complete(&mgr).unwrap();
        }
        assert!(indexes.join("rotation.state").exists());
        assert!(read_encryption_state(&indexes).unwrap().is_none());

        // Reopen WITH the cold tier: phase-1 resume flips + clears (all settled).
        {
            let db =
                AletheiaDB::with_unified_config(plaintext_durable_config_with_cold(dir.path()))
                    .unwrap();
            assert!(db.wal.is_encrypted());
            let tiered = db.historical.read().tiered_storage_arc().unwrap();
            assert!(
                tiered
                    .cold_storage()
                    .raw_node_value_is_acv1_for_test(cold_vid),
                "cold value stays ACV1"
            );
        }
        let state = read_encryption_state(&indexes).unwrap().unwrap();
        assert!(state.enabled, "authority flipped by phase-1 resume");
        assert!(!indexes.join("rotation.state").exists(), "ledger cleared");
    }
}
