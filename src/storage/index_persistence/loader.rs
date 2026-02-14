//! Index loading and directory management.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::Result;
use super::formats::IndexManifest;
use super::manifest::{load_manifest, save_manifest};
use super::strings::{load_string_interner, restore_string_interner, save_string_interner};

/// Manages index persistence directory structure.
pub struct IndexPersistenceManager {
    /// Base directory for all index files
    base_path: PathBuf,
}

impl IndexPersistenceManager {
    /// Create a new persistence manager.
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
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
        // 1. Load and restore string interner FIRST (if it exists)
        // This must happen before manifest check to enable recovery when manifest is missing
        // but other index files exist (partial save failure scenario)
        let interner_path = self.interner_path();
        let interner_was_loaded = if interner_path.exists() {
            let interner_data = load_string_interner(&interner_path)?;
            restore_string_interner(&interner_data)?;
            true
        } else {
            false
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
                return Ok(super::formats::IndexManifest::new(0));
            }

            // No index files exist at all - this is expected on first run
            return Err(super::error::IndexPersistenceError::MissingIndex {
                name: "manifest.idx".to_string(),
            });
        }

        // 3. Load manifest
        let manifest = load_manifest(&manifest_path)?;

        Ok(manifest)
    }

    /// Save the manifest.
    pub fn save_manifest(&self, manifest: &IndexManifest) -> Result<()> {
        self.ensure_directories()?;
        save_manifest(manifest, &self.manifest_path())
    }

    /// Save the string interner.
    pub fn save_string_interner(&self) -> Result<()> {
        self.ensure_directories()?;
        save_string_interner(&self.interner_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::GLOBAL_INTERNER;
    use tempfile::tempdir;

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
