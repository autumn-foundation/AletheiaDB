//! Plaintext → encrypted-at-rest migration engine (Issue #3616 PR3).
//!
//! This is the engine behind `aletheia encryption enable`: it takes a database
//! that was created **plaintext** and migrates every in-scope at-rest layer
//! through the cipher, then flips the durable
//! [`encryption.state`](crate::db::encryption_state) authority so the *next*
//! [`open()`](crate::AletheiaDB::open) comes up fully encrypted.
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
//! 1. **Quiesce the background index-persistence thread FIRST.** While a live
//!    keyring is still `None`, a background persist could overwrite a
//!    freshly-encrypted file with plaintext. The thread must be stopped before
//!    any keyring is installed.
//! 2. **Migrate all in-scope layers**, recording each layer's progress as a
//!    per-field [`LayerStatus`](crate::db::rotation) in the durable rotation
//!    ledger so a crash mid-migration resumes idempotently:
//!    * **WAL** — install the DEK keyring via the PR2 seam, force-roll to a
//!      fresh v16 (EVEN-parity) encrypted segment.
//!    * **Index** — wrap every plaintext index file into the `AEIX` encrypted
//!      format under the index DEK. (NOTE: this is a *new* plaintext→AEIX wrap
//!      pass — the #488 [`IndexKeyRotation::re_encrypt`] engine deliberately
//!      SKIPS plaintext files, so it cannot be reused as-is here.)
//!    * **Checkpoint** — rides the index DEK + index file format; covered by the
//!      index pass.
//!    * **Cold** — wrap every bare stored value into the `ACV1` cold-wrap format
//!      under the cold DEK (wrap-only; a fresh enable is not a re-key).
//! 3. **Flip the [`encryption.state`] authority to `enabled` BEFORE clearing the
//!    ledger.** This binding order closes the crash gap: were the ledger cleared
//!    first and a crash struck before the authority flip, the next `open()`
//!    would read the unchanged plaintext config over now-encrypted bytes and
//!    fail (or worse). With the authority flipped first, a crash between the two
//!    steps leaves a still-present ledger that [`resume_pending_enable`]
//!    reconciles on the next `open()`.
//! 4. **Clear the ledger.**
//!
//! # In scope vs deferred
//!
//! In scope: WAL, index, checkpoint, cold. **Deferred (NOT migrated by v1):**
//! `.albk` backups, `snapshots.json`, auth `keys.json`, `schema_constraints.dat`.
//!
//! # Crash resume
//!
//! An interrupted enable has NOT yet flipped the authority, so
//! `config.encryption.enabled` is still `false` — which means the existing
//! rotation resume paths ([`resume_pending_rotation`](crate::db::rotation), which
//! gate on `enabled == true`) will NOT fire. That is exactly why this module
//! ships its own [`resume_pending_enable`] reconciler, wired into `open()`.

use crate::core::error::{Result, StorageError};
use crate::db::AletheiaDB;
use crate::encryption::config::KeyProviderConfig;

/// Outcome of a completed plaintext → encrypted enable migration.
///
/// Each flag reports whether that at-rest layer actually had bytes migrated
/// through the cipher (a layer that was empty / not configured is `false`). The
/// key-source reference that was recorded into the durable
/// [`encryption.state`](crate::db::encryption_state) authority is echoed so the
/// operator can confirm what the next `open()` will consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableReport {
    /// WAL segments rolled to the encrypted (v16) format.
    pub wal_migrated: bool,
    /// Plaintext index files wrapped into the `AEIX` format.
    pub index_migrated: bool,
    /// Checkpoint files covered (rides the index DEK + format).
    pub checkpoint_migrated: bool,
    /// Cold-storage bare values wrapped into the `ACV1` format.
    pub cold_migrated: bool,
    /// The non-secret key-source reference recorded into the authority
    /// (File path / Env var name — never key bytes).
    pub key_source: KeyProviderConfig,
}

impl AletheiaDB {
    /// Migrate this **plaintext** database to encrypted-at-rest under
    /// `key_source`, in place, crash-consistently (Issue #3616 PR3).
    ///
    /// # ⚠️ LOUD REOPEN CONTRACT — read before calling
    ///
    /// This is a **reopen-centric** migration. When this call returns `Ok`, the
    /// handle you are holding is in a deliberately **partial** runtime state:
    ///
    /// * **WAL** is encrypted **live** — the DEK keyring has been installed and
    ///   every subsequent WAL append is written encrypted.
    /// * **Index / checkpoint / cold persistence is QUIESCED** — the background
    ///   index-persistence thread was stopped as step 1 and is **not restarted**
    ///   by this call. The on-disk index/checkpoint/cold bytes have been
    ///   migrated, but this handle will **not** resume periodic encrypted
    ///   persistence.
    ///
    /// **You MUST reopen the database** (drop this handle and call
    /// [`AletheiaDB::open`]) to get a fully-operational encrypted instance: the
    /// next `open()` reads the flipped [`encryption.state`](crate::db::encryption_state)
    /// authority, brings every layer up under the cipher, and restarts the
    /// background persistence thread. Continuing to write through the returned
    /// handle beyond draining/reopening is **not supported** in v1.
    ///
    /// # Errors
    ///
    /// Returns a structured error (never a partial silent success) when:
    /// * index persistence is not enabled (an ephemeral / in-memory database
    ///   cannot be migrated — it has no durable at-rest bytes or authority file);
    /// * the database is already encrypted (`FAILED_PRECONDITION` — use key
    ///   rotation, not enable);
    /// * `key_source` is a Passphrase/KMS/Vault backend (the authority + ledger
    ///   can only persist a File/Env reference — mirrors the rotation ledger);
    /// * any layer migration or the durable authority flip fails.
    ///
    /// On any error, the binding order guarantees the database is left either
    /// fully plaintext (authority never flipped) or resumable via
    /// [`resume_pending_enable`] on the next `open()` — never a silently
    /// half-encrypted state that reads as plaintext.
    pub fn enable_encryption(&self, key_source: KeyProviderConfig) -> Result<EnableReport> {
        // Drive the whole migration off per-field `LayerStatus` checks in the
        // rotation ledger — NEVER off a positional index or a count of layers.
        // The layer set can grow; each step must consult its own field so a
        // future layer addition cannot silently skip migration or resume.

        // === Step 0: preconditions ===================================================
        // TODO(#3616 PR3): require `self.persistence_manager.is_some()` (durable
        //   db); reject ephemeral with a clear error.
        // TODO(#3616 PR3): reject if already encrypted — `self.encryption_manager
        //   .is_some()` or `self.wal.is_encrypted()` or an `enabled` authority on
        //   disk — mapping to FAILED_PRECONDITION (enable != rotate).
        // TODO(#3616 PR3): reject Passphrase/KMS/Vault `key_source` up front (only
        //   File/Env round-trip through the authority + ledger).
        // TODO(#3616 PR3): source the MEK from `key_source` and derive the
        //   per-layer DEKs (`MekKeyset::derive`), holding them only in `Zeroizing`.

        // === Step 1: quiesce the background index-persistence thread =================
        // TODO(#3616 PR3): stop + join the background persistence worker BEFORE
        //   any keyring install, so a still-`None` live keyring can never
        //   overwrite a freshly-encrypted file with plaintext. NOTE: the current
        //   `persistence_thread_stopped` flag is terminal (shutdown-only), not a
        //   pause — this step needs a pausable worker or a stop→migrate→respawn
        //   dance (open design point; see contract notes).

        // === Step 2: migrate all in-scope layers (record per-field LayerStatus) ======
        // TODO(#3616 PR3): write an `enable`-scope rotation ledger (all in-scope
        //   layers `Pending`) DURABLY before touching any layer.
        // TODO(#3616 PR3): WAL — build a `WalKeyring` from the WAL DEK and call
        //   `self.wal.install_wal_keyring(..)` (PR2 seam, None→Some), then
        //   force-roll; mark `layer.wal = Complete`.
        // TODO(#3616 PR3): Index — NEW plaintext→AEIX wrap pass over the index
        //   tree (NOT `IndexKeyRotation::re_encrypt`, which skips plaintext);
        //   mark `layer.index = Complete`. Checkpoint rides this → `Complete`.
        // TODO(#3616 PR3): Cold — wrap bare values into `ACV1` under the cold DEK
        //   (wrap-only); mark `layer.cold = Complete`. No-op when no cold store.

        // === Step 3: flip the authority BEFORE clearing the ledger (binding order) ===
        // TODO(#3616 PR3): `write_encryption_state_durable(data_dir,
        //   &EncryptionState::enabled(key_source))` — the crux ordering. A crash
        //   after this but before the clear resumes via `resume_pending_enable`.

        // === Step 4: clear the ledger ================================================
        // TODO(#3616 PR3): `clear_rotation_state(manager)` once the authority is
        //   durably flipped.

        let _ = &key_source;
        Err(StorageError::PersistenceError(
            "enable_encryption: the plaintext → encrypted migration engine is not yet implemented \
             (Issue #3616 PR3 — skeleton only). The public API surface, the LOUD reopen contract, \
             and the crash-safe step ordering are in place; the per-layer migration passes and the \
             authority flip land in a follow-up commit on this PR."
                .to_string(),
        )
        .into())
    }

    /// Resume an interrupted plaintext → encrypted enable migration at
    /// `open()` time, if one is pending (Issue #3616 PR3).
    ///
    /// Wired into [`with_unified_config`](crate::AletheiaDB::with_unified_config)
    /// alongside the rotation resume paths, but distinct from them: an interrupted
    /// enable has NOT yet flipped the [`encryption.state`](crate::db::encryption_state)
    /// authority, so `config.encryption.enabled` is still `false` and
    /// [`resume_pending_rotation`](crate::db::rotation) (which gates on
    /// `enabled == true`) never fires. This reconciler recognizes the
    /// enable-scope rotation ledger and completes the migration idempotently,
    /// driving off each layer's per-field `LayerStatus`.
    ///
    /// **Guarded no-op:** returns `Ok(())` immediately when no durable
    /// pending-enable ledger exists (the overwhelmingly common startup path),
    /// so wiring it unconditionally into `open()` costs at most one ledger probe.
    ///
    /// # Errors
    ///
    /// Propagates a fail-closed error if a present ledger is corrupt or a resume
    /// pass fails; a corrupt/undecryptable state must abort startup loudly rather
    /// than be treated as "no migration pending".
    pub(crate) fn resume_pending_enable(&self) -> Result<()> {
        // Guard: only durable databases (index persistence enabled) can carry a
        // pending-enable ledger; an ephemeral db has nothing to reconcile.
        let Some(_manager) = self.persistence_manager.as_ref() else {
            return Ok(());
        };

        // TODO(#3616 PR3): read the rotation ledger (fail-closed) and detect an
        //   `enable`-scope pending migration; return Ok(()) when absent so this
        //   stays a true no-op on the normal startup path.
        // TODO(#3616 PR3): for each layer whose `LayerStatus` is `Pending`, run
        //   its idempotent migration pass to `Complete` (WAL install+roll, index
        //   plaintext→AEIX wrap, cold bare→ACV1 wrap), driven off the per-field
        //   status — NEVER a positional/count assumption about the layer set.
        // TODO(#3616 PR3): once every in-scope layer is `Complete`, flip the
        //   `encryption.state` authority to `enabled` (if not already flipped),
        //   THEN clear the ledger — the same binding order as the forward engine.
        Ok(())
    }
}
