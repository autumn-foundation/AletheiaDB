//! Index loading and directory management.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;

use super::common::IndexKeyring;
use super::error::Result;
use super::formats::IndexManifest;
use super::manifest::{load_manifest_with_keyring, save_manifest_with_keyring};
use super::strings::{
    InternerRemap, load_string_interner_with_keyring, restore_string_interner,
    save_string_interner_with_keyring,
};
use crate::encryption::cipher::Cipher;

/// Manages index persistence directory structure.
pub struct IndexPersistenceManager {
    /// Base directory for all index files
    base_path: PathBuf,
    /// Optional key-version-addressable cipher set for encryption-at-rest of
    /// index files (Issue #481 single-generation; Issue #488 rotation).
    /// `None` means indexes are persisted/read as plaintext (default,
    /// back-compatible). When `Some`, files are written with the encrypted
    /// index header stamped with the keyring's current `key_version`, and read
    /// back by dispatching on each file's header `key_version` (so a mix of
    /// old- and new-key files during a rotation reads correctly). A legacy
    /// plaintext file is still read correctly via header sniffing.
    ///
    /// The cell is an [`ArcSwapOption`] so a keyring can be **installed at
    /// runtime** (`None` -> `Some`, Issue #3708) into a live plaintext manager —
    /// the index-tier mirror of the WAL's `install_wal_keyring` seam (#3669) and
    /// the cold tier's
    /// [`install_cold_keyring`](crate::storage::redb_cold_storage::RedbColdStorage::install_cold_keyring)
    /// (#3733) — without reopening. Every persist/load path `load()`s it
    /// lock-free through [`Self::keyring`], so an install is observed atomically
    /// and never as a torn read; the install itself is serialized by
    /// [`install_lock`](Self::install_lock).
    keyring: ArcSwapOption<IndexKeyring>,
    /// Serializes runtime keyring installs (Issue #3708) so two concurrent
    /// installers cannot both observe the cell as `None`, both pass the presence
    /// check, and both store -- the second silently replacing the first keyring.
    /// Held for the entire [`install_index_keyring`](Self::install_index_keyring)
    /// body (presence check + store) so a second concurrent installer blocks,
    /// then observes `Some`, and returns the existing rejection `Err`. A private
    /// LEAF taken only at the top of install; the hot persist/load paths never
    /// touch it, and the install body acquires no other AletheiaDB primitive
    /// while holding it (CLAUDE.md lock order).
    install_lock: Mutex<()>,
}

impl IndexPersistenceManager {
    /// Create a new persistence manager (no encryption).
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            keyring: ArcSwapOption::empty(),
            install_lock: Mutex::new(()),
        }
    }

    /// Create a new persistence manager with an optional index cipher.
    ///
    /// Passing `None` is equivalent to [`IndexPersistenceManager::new`]. A
    /// `Some(cipher)` builds a single-generation keyring (the non-rotation
    /// path), byte-for-byte identical to Issue #481.
    pub fn with_cipher(
        base_path: impl Into<PathBuf>,
        index_cipher: Option<Arc<dyn Cipher>>,
    ) -> Self {
        Self {
            base_path: base_path.into(),
            keyring: ArcSwapOption::new(index_cipher.map(|c| Arc::new(IndexKeyring::single(c)))),
            install_lock: Mutex::new(()),
        }
    }

    /// Like [`Self::with_cipher`] but PROVISIONS the single-generation keyring's
    /// write-stamp version to `key_version` (Issue #488 version-provisioning).
    ///
    /// The durable `open()` path resolves `key_version` from the max on-disk key
    /// version (index-file AEIX headers folded with WAL segment headers) so a
    /// rotated-then-reopened database reports the correct
    /// [`current_version`](super::common::IndexKeyring::current_version) instead
    /// of a hard-coded 1. Reads stay `match_any` (the sole cipher decrypts every
    /// header version, byte-identical to [`Self::with_cipher`]); only the
    /// stamped/reported version differs. Passing `None` is plaintext (no
    /// keyring), exactly like [`Self::with_cipher`].
    pub fn with_cipher_versioned(
        base_path: impl Into<PathBuf>,
        index_cipher: Option<Arc<dyn Cipher>>,
        key_version: u32,
    ) -> Self {
        Self {
            base_path: base_path.into(),
            keyring: ArcSwapOption::new(
                index_cipher.map(|c| Arc::new(IndexKeyring::single_versioned(c, key_version))),
            ),
            install_lock: Mutex::new(()),
        }
    }

    /// Create a new persistence manager with an explicit
    /// [`IndexKeyring`](super::common::IndexKeyring) (Issue #488 key rotation).
    /// Test-only: production builds the manager via [`Self::with_cipher`] and
    /// mutates the shared keyring in place during rotation.
    #[cfg(test)]
    pub(crate) fn with_keyring(
        base_path: impl Into<PathBuf>,
        keyring: Option<IndexKeyring>,
    ) -> Self {
        Self {
            base_path: base_path.into(),
            keyring: ArcSwapOption::new(keyring.map(Arc::new)),
            install_lock: Mutex::new(()),
        }
    }

    /// The index keyring, if encryption-at-rest is enabled.
    ///
    /// Returns an **owned** clone of the keyring (a cheap `Arc`-bump — the
    /// [`IndexKeyring`] is `Arc<RwLock<..>>`-backed, so the clone shares the same
    /// generations and observes concurrent `add_generation`/`retire`). The cell
    /// is `load()`ed lock-free, so a concurrent runtime install (Issue #3708) is
    /// observed atomically as either the old `None` or the fully-valid `Some`,
    /// never a torn read. Callers passing the keyring into a
    /// `*_with_keyring(Option<&IndexKeyring>)` persist/load fn use
    /// `keyring().as_ref()`.
    pub(crate) fn keyring(&self) -> Option<IndexKeyring> {
        self.keyring.load_full().map(|arc| (*arc).clone())
    }

    /// Install an index keyring into a live plaintext manager at runtime
    /// (Issue #3708): the `None` -> `Some` transition that flips
    /// encryption-at-rest on **without reopening** the database. The index-tier
    /// mirror of the WAL's `install_wal_keyring` (#3669) and the cold tier's
    /// [`install_cold_keyring`](crate::storage::redb_cold_storage::RedbColdStorage::install_cold_keyring)
    /// (#3733) seams. Unlike the WAL there is no on-disk seal step — the index
    /// header is per-file and sniffed on read, so pre-install plaintext files
    /// keep reading correctly and post-install writes are AEIX-encrypted.
    ///
    /// Installing the keyring alone does **not** retroactively encrypt
    /// already-persisted plaintext index files; the hot-live enable driver runs
    /// a separate wrap pass (`reencrypt`) after installing. Presence is a
    /// one-way transition: key rotation extends an already-installed keyring via
    /// its shared `add_generation` handle, not this seam.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::IndexKeyringAlreadyInstalled`] if a keyring is
    /// already installed (a double-install) — a caller precondition failure the
    /// MCP surface maps to `FAILED_PRECONDITION` (non-retriable), never a silent
    /// replace.
    ///
    /// [`StorageError::IndexKeyringAlreadyInstalled`]:
    /// crate::core::error::StorageError::IndexKeyringAlreadyInstalled
    // The production consumer is the hot-live plaintext -> encrypted enable
    // engine (Issue #3708), which drives this from `enable_encryption` after the
    // WAL's `install_wal_keyring` and the index plaintext -> `AEIX` wrap pass,
    // mirroring the WAL/cold seams.
    pub(crate) fn install_index_keyring(
        &self,
        keyring: IndexKeyring,
    ) -> crate::core::error::Result<()> {
        // Serialize the ENTIRE install -- presence check + store -- under a
        // dedicated leaf mutex. Without it two concurrent installers could both
        // observe `None`, both pass the check, and both store, the second
        // silently replacing the first keyring.
        let _install_guard = self.install_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject a double-install: presence is a one-way None -> Some transition.
        if self.keyring.load().is_some() {
            return Err(
                crate::core::error::StorageError::IndexKeyringAlreadyInstalled {
                    reason: "an index keyring is already installed; runtime install only \
                         supports the plaintext -> encrypted (None -> Some) transition \
                         (key rotation extends the keyring via add_generation)"
                        .to_string(),
                }
                .into(),
            );
        }

        self.keyring.store(Some(Arc::new(keyring)));
        Ok(())
    }

    /// Get the base path.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Get the indexes directory path.
    pub fn indexes_path(&self) -> PathBuf {
        self.base_path.join("indexes")
    }

    /// Get the manifest file path.
    pub fn manifest_path(&self) -> PathBuf {
        self.indexes_path().join("manifest.idx")
    }

    /// Get the string interner file path.
    pub fn interner_path(&self) -> PathBuf {
        self.indexes_path().join("strings").join("interner.idx")
    }

    /// Get the graph index directory path.
    pub fn graph_path(&self) -> PathBuf {
        self.indexes_path().join("graph")
    }

    /// Get the temporal index directory path.
    pub fn temporal_path(&self) -> PathBuf {
        self.indexes_path().join("temporal")
    }

    /// Get the vector index directory for a property.
    pub fn vector_path(&self, property_name: &str) -> PathBuf {
        self.indexes_path().join("vector").join(property_name)
    }

    /// Ensure all required directories exist.
    pub fn ensure_directories(&self) -> Result<()> {
        fs::create_dir_all(self.indexes_path().join("strings"))?;
        fs::create_dir_all(self.graph_path())?;
        fs::create_dir_all(self.temporal_path())?;
        fs::create_dir_all(self.indexes_path().join("vector"))?;
        Ok(())
    }

    /// Check if indexes exist on disk.
    pub fn indexes_exist(&self) -> bool {
        self.manifest_path().exists()
    }

    /// Load all indexes from disk.
    ///
    /// Load order:
    /// 1. String interner first (if exists) - required for all other indexes
    /// 2. Manifest (if exists)
    /// 3. Other indexes can be loaded in parallel after this
    ///
    /// # Resilient Recovery
    ///
    /// This function is designed for best-effort recovery from partial save failures.
    /// If the manifest is missing but the string interner exists, we still load the
    /// interner so that graph/temporal restoration can proceed. This handles the case
    /// where a crash occurred after saving indexes but before saving the manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The manifest file is missing AND no other index files exist
    /// - Failed to load or restore string interner
    /// - Failed to load manifest (if it exists)
    pub fn load_manifest_and_strings(&self) -> Result<IndexManifest> {
        let (manifest, _remap) = self.load_manifest_and_strings_with_remap()?;
        Ok(manifest)
    }

    /// Load the manifest and restore the string interner, returning the
    /// file-id -> live-id [`InternerRemap`] alongside the manifest.
    ///
    /// This is the Issue #3490-aware entry point: callers that go on to restore
    /// the graph and/or temporal indexes (e.g. `load_indexes_startup`) MUST use
    /// this variant and translate the loaded index data through the returned
    /// remap (see [`InternerRemap::remap_graph_index_data`] /
    /// [`InternerRemap::remap_temporal_index_data`]) before resolving any
    /// persisted interner id. The plain [`Self::load_manifest_and_strings`]
    /// wrapper discards the remap and is only safe for callers that do not
    /// resolve persisted interner ids afterward, or that restore against a
    /// pristine interner (where the remap is identity).
    ///
    /// When no interner file exists, the returned remap is
    /// [`InternerRemap::identity`] (ids pass through unchanged), preserving the
    /// legacy behavior for that best-effort path.
    pub fn load_manifest_and_strings_with_remap(&self) -> Result<(IndexManifest, InternerRemap)> {
        // 1. Load and restore string interner FIRST (if it exists)
        // This must happen before manifest check to enable recovery when manifest is missing
        // but other index files exist (partial save failure scenario)
        let interner_path = self.interner_path();
        let (interner_was_loaded, remap) = if interner_path.exists() {
            // Decrypt the persisted interner with the configured index cipher
            // (Issue #481); `restore_string_interner` returns the file-id ->
            // live-id remap (Issue #3490) the caller threads through the graph /
            // temporal index data.
            let interner_data =
                load_string_interner_with_keyring(&interner_path, self.keyring().as_ref())?;
            let remap = restore_string_interner(&interner_data)?;
            (true, remap)
        } else {
            (false, InternerRemap::identity())
        };

        // 2. Check if manifest exists
        let manifest_path = self.manifest_path();
        if !manifest_path.exists() {
            // Manifest is missing. If we loaded the interner, we can attempt recovery.
            if interner_was_loaded {
                // Return a default manifest - best-effort recovery mode
                // The caller (load_indexes_startup) will attempt to load individual index files
                eprintln!(
                    "Warning: Manifest missing but string interner exists - attempting best-effort recovery"
                );
                return Ok((super::formats::IndexManifest::new(0), remap));
            }

            // No index files exist at all - this is expected on first run
            return Err(super::error::IndexPersistenceError::MissingIndex {
                name: "manifest.idx".to_string(),
            });
        }

        // 3. Load manifest
        let manifest = load_manifest_with_keyring(&manifest_path, self.keyring().as_ref())?;

        Ok((manifest, remap))
    }

    /// Save the manifest.
    pub fn save_manifest(&self, manifest: &IndexManifest) -> Result<()> {
        self.ensure_directories()?;
        save_manifest_with_keyring(manifest, &self.manifest_path(), self.keyring().as_ref())
    }

    /// Save the string interner, returning the exact number of strings written.
    pub fn save_string_interner(&self) -> Result<u64> {
        self.ensure_directories()?;
        save_string_interner_with_keyring(&self.interner_path(), self.keyring().as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GLOBAL_INTERNER;
    use tempfile::tempdir;

    fn enc_cipher(seed: u8) -> Arc<dyn Cipher> {
        use crate::encryption::Aes256GcmCipher;
        use zeroize::Zeroizing;
        let mut key = Zeroizing::new([0u8; 32]);
        key[0] = seed;
        Arc::new(Aes256GcmCipher::new(&key))
    }

    #[test]
    fn manager_load_path_dispatches_on_key_version_in_mixed_dir() {
        // Issue #488: the manager's load path must read a MIX of old-key and
        // new-key index files via keyring dispatch. We write the interner under
        // the OLD key (v1) and the manifest under the NEW key (v2), then load
        // both through a two-generation keyring manager.
        let dir = tempdir().unwrap();
        let (old, new) = (enc_cipher(0xE1), enc_cipher(0xE2));

        GLOBAL_INTERNER.intern("mixed_dir_label").unwrap();

        // Interner: OLD key, key_version 1.
        let mgr_old = IndexPersistenceManager::with_cipher(dir.path(), Some(old.clone()));
        mgr_old.save_string_interner().unwrap();

        // Manifest: NEW key, key_version 2 (strict single-gen keyring at v2).
        let ring_new = IndexKeyring::single_versioned(new.clone(), 2);
        let mgr_new = IndexPersistenceManager::with_keyring(dir.path(), Some(ring_new));
        mgr_new.save_manifest(&IndexManifest::new(4242)).unwrap();

        // Sanity: the two files really are stamped with different key_versions.
        let manifest_raw = std::fs::read(mgr_old.manifest_path()).unwrap();
        let interner_raw = std::fs::read(mgr_old.interner_path()).unwrap();
        assert_eq!(
            super::super::common::index_file_key_version(&manifest_raw),
            Some(2)
        );
        assert_eq!(
            super::super::common::index_file_key_version(&interner_raw),
            Some(1)
        );

        // A two-generation keyring (old v1 + new v2, current v2) loads BOTH.
        let ring_mixed = IndexKeyring::single_versioned(old, 1);
        ring_mixed.add_generation(2, new);
        let mgr_mixed = IndexPersistenceManager::with_keyring(dir.path(), Some(ring_mixed));
        let manifest = mgr_mixed.load_manifest_and_strings().unwrap();
        assert_eq!(manifest.lsn, 4242, "new-key manifest read via dispatch");
    }

    #[test]
    fn test_persistence_manager_paths() {
        let dir = tempdir().unwrap();
        let manager = IndexPersistenceManager::new(dir.path());

        assert_eq!(manager.indexes_path(), dir.path().join("indexes"));
        assert_eq!(
            manager.manifest_path(),
            dir.path().join("indexes").join("manifest.idx")
        );
        assert_eq!(
            manager.vector_path("embedding"),
            dir.path().join("indexes").join("vector").join("embedding")
        );
    }

    #[test]
    fn test_ensure_directories() {
        let dir = tempdir().unwrap();
        let manager = IndexPersistenceManager::new(dir.path());

        manager.ensure_directories().unwrap();

        assert!(manager.indexes_path().join("strings").exists());
        assert!(manager.graph_path().exists());
        assert!(manager.temporal_path().exists());
    }

    #[test]
    fn test_save_and_load_manifest() {
        let dir = tempdir().unwrap();
        let manager = IndexPersistenceManager::new(dir.path());

        // Intern some strings first
        GLOBAL_INTERNER.intern("test_label").unwrap();

        // Save interner
        manager.save_string_interner().unwrap();

        // Save manifest
        let manifest = IndexManifest::new(100);
        manager.save_manifest(&manifest).unwrap();

        // Verify files exist
        assert!(manager.manifest_path().exists());
        assert!(manager.interner_path().exists());

        // Load back
        let loaded = manager.load_manifest_and_strings().unwrap();
        assert_eq!(loaded.lsn, 100);
    }

    // ── Issue #3708: runtime keyring-install seam ──────────────────────────────

    /// The `None -> Some` runtime install flips encryption-at-rest on a live
    /// plaintext manager: post-install index writes carry the `AEIX` header,
    /// while pre-install plaintext files still read back (header sniffing).
    #[test]
    fn install_index_keyring_none_to_some_transition() {
        use crate::storage::index_persistence::common::is_encrypted_index;

        let dir = tempdir().unwrap();

        // A plaintext-opened manager reports no keyring and writes cleartext.
        let manager = IndexPersistenceManager::new(dir.path());
        assert!(
            manager.keyring().is_none(),
            "a plain manager has no keyring"
        );

        GLOBAL_INTERNER.intern("install_seam_label").unwrap();
        manager.save_string_interner().unwrap();
        manager.save_manifest(&IndexManifest::new(11)).unwrap();

        // Pre-install files are plaintext at rest.
        let interner_raw = std::fs::read(manager.interner_path()).unwrap();
        assert!(
            !is_encrypted_index(&interner_raw),
            "pre-install interner must be plaintext at rest"
        );

        // Install a keyring at runtime (the plaintext -> encrypted enable seam).
        manager
            .install_index_keyring(IndexKeyring::single(enc_cipher(0x51)))
            .unwrap();
        assert!(
            manager.keyring().is_some(),
            "installing a keyring must flip the manager to encrypted"
        );

        // A manifest written AFTER the install is AEIX-encrypted at rest.
        manager.save_manifest(&IndexManifest::new(22)).unwrap();
        let manifest_raw = std::fs::read(manager.manifest_path()).unwrap();
        assert!(
            is_encrypted_index(&manifest_raw),
            "post-install manifest must be AEIX-encrypted at rest (found plaintext)"
        );

        // A runtime install must encrypt MORE than just the manifest: re-saving
        // the string interner after the install (the same re-save the manifest
        // gets) must also carry the AEIX header, proving the keyring is threaded
        // through every persist path — not just the manifest write.
        manager.save_string_interner().unwrap();
        let interner_raw_post = std::fs::read(manager.interner_path()).unwrap();
        assert!(
            is_encrypted_index(&interner_raw_post),
            "post-install string interner must be AEIX-encrypted at rest (found plaintext)"
        );

        // The now-encrypted manifest and re-saved interner both round-trip
        // through the installed keyring (header sniffing selects the encrypted
        // read path for each).
        let manifest = manager.load_manifest_and_strings().unwrap();
        assert_eq!(
            manifest.lsn, 22,
            "post-install encrypted manifest must round-trip"
        );
    }

    /// Fail-closed double-install: installing when a keyring is already present is
    /// rejected with `IndexKeyringAlreadyInstalled` (never a silent replace), and
    /// data written under the first keyring still decrypts afterward.
    #[test]
    fn install_index_keyring_twice_is_rejected() {
        use crate::core::error::{Error, StorageError};

        let dir = tempdir().unwrap();
        let manager = IndexPersistenceManager::new(dir.path());

        // Install the first keyring and persist a manifest under it.
        manager
            .install_index_keyring(IndexKeyring::single(enc_cipher(0x11)))
            .unwrap();
        GLOBAL_INTERNER.intern("twice_label").unwrap();
        manager.save_manifest(&IndexManifest::new(7)).unwrap();

        // A second install with a DIFFERENT cipher must be rejected, not applied.
        let err = manager
            .install_index_keyring(IndexKeyring::single(enc_cipher(0x22)))
            .expect_err("a second install must be rejected, not silently applied");
        assert!(
            matches!(
                err,
                Error::Storage(StorageError::IndexKeyringAlreadyInstalled { .. })
            ),
            "double-install must return IndexKeyringAlreadyInstalled, got: {err:?}"
        );

        // Still encrypted under the FIRST keyring: had the 0x22 keyring silently
        // replaced it, the 0x11-encrypted manifest would fail to decrypt.
        assert!(manager.keyring().is_some());
        let manifest = manager.load_manifest_and_strings().unwrap();
        assert_eq!(
            manifest.lsn, 7,
            "a rejected double-install must not lose (or re-key) existing data"
        );
    }

    /// Torn-read-free atomic visibility of the install across concurrent readers.
    /// The `ArcSwapOption` presence cell makes a *literal* torn read structurally
    /// impossible (a load atomically observes the whole pointer), so what this
    /// test actually validates is cross-thread STORE VISIBILITY: while one thread
    /// installs, concurrent loaders read `keyring()` and must always see either
    /// the old `None` or a fully-valid `Some` (a resolvable current generation),
    /// never a partial/half-published state. The saw_none/saw_some records prove
    /// the `None -> Some` store is actually made visible across threads.
    /// (Name kept aligned with the WAL/cold sibling tests.)
    #[test]
    fn install_index_keyring_no_torn_reads_on_cell() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let manager = Arc::new(IndexPersistenceManager::new(dir.path()));

        let loaders = 6usize;
        let stop = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(loaders + 1));

        let mut handles = Vec::new();
        for _ in 0..loaders {
            let manager = Arc::clone(&manager);
            let stop = Arc::clone(&stop);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut saw_none = false;
                let mut saw_some = false;
                barrier.wait();
                while !stop.load(AtOrd::Relaxed) {
                    match manager.keyring() {
                        Some(k) => {
                            saw_some = true;
                            // Any observed keyring must be fully constructed: an
                            // installed keyring always resolves a current cipher.
                            assert!(
                                k.current().is_some(),
                                "an installed keyring must resolve a current generation"
                            );
                        }
                        None => saw_none = true,
                    }
                }
                (saw_none, saw_some)
            }));
        }

        barrier.wait();
        // Let loaders observe the pre-install `None` before the store flips it.
        std::thread::sleep(std::time::Duration::from_millis(5));
        manager
            .install_index_keyring(IndexKeyring::single(enc_cipher(0x71)))
            .unwrap();
        // Keep loaders running long enough to observe the post-install `Some`.
        std::thread::sleep(std::time::Duration::from_millis(5));
        stop.store(true, AtOrd::Relaxed);

        let mut any_saw_none = false;
        let mut any_saw_some = false;
        for h in handles {
            let (saw_none, saw_some) = h.join().unwrap();
            any_saw_none |= saw_none;
            any_saw_some |= saw_some;
        }
        assert!(
            any_saw_none,
            "loaders must have observed the pre-install None state"
        );
        assert!(
            any_saw_some,
            "loaders must have observed the post-install Some state \
             (proving the store is seen across threads)"
        );
        assert!(
            manager.keyring().is_some(),
            "install must be visible after the store"
        );
    }

    /// Concurrent double-install TOCTOU: two threads race `install_index_keyring`
    /// on the same manager. Without the install lock both could observe `None`,
    /// both pass the presence check, and both store — the second silently
    /// replacing the first. With the lock, EXACTLY one returns `Ok` and one
    /// returns `Err`, and the manager ends encrypted.
    #[test]
    fn install_index_keyring_concurrent_double_install_rejected() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering as AtOrd};

        let dir = tempdir().unwrap();
        let manager = Arc::new(IndexPersistenceManager::new(dir.path()));

        let ok_count = Arc::new(AtomicUsize::new(0));
        let err_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();
        for seed in [0x11u8, 0x22u8] {
            let manager = Arc::clone(&manager);
            let ok_count = Arc::clone(&ok_count);
            let err_count = Arc::clone(&err_count);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                match manager.install_index_keyring(IndexKeyring::single(enc_cipher(seed))) {
                    Ok(()) => ok_count.fetch_add(1, AtOrd::Relaxed),
                    Err(_) => err_count.fetch_add(1, AtOrd::Relaxed),
                };
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            ok_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must succeed"
        );
        assert_eq!(
            err_count.load(AtOrd::Relaxed),
            1,
            "exactly one concurrent install must be rejected"
        );
        assert!(
            manager.keyring().is_some(),
            "the survivor keyring must be installed"
        );
    }
}
