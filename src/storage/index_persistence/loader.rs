//! Index loading and directory management.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    keyring: Option<IndexKeyring>,
}

impl IndexPersistenceManager {
    /// Create a new persistence manager (no encryption).
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            keyring: None,
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
            keyring: index_cipher.map(IndexKeyring::single),
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
            keyring,
        }
    }

    /// The index keyring, if encryption-at-rest is enabled.
    pub(crate) fn keyring(&self) -> Option<&IndexKeyring> {
        self.keyring.as_ref()
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
            let interner_data = load_string_interner_with_keyring(&interner_path, self.keyring())?;
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
        let manifest = load_manifest_with_keyring(&manifest_path, self.keyring())?;

        Ok((manifest, remap))
    }

    /// Save the manifest.
    pub fn save_manifest(&self, manifest: &IndexManifest) -> Result<()> {
        self.ensure_directories()?;
        save_manifest_with_keyring(manifest, &self.manifest_path(), self.keyring())
    }

    /// Save the string interner.
    pub fn save_string_interner(&self) -> Result<()> {
        self.ensure_directories()?;
        save_string_interner_with_keyring(&self.interner_path(), self.keyring())
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
}
