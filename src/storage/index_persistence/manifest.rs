//! Index manifest persistence.
//!
//! The [`IndexManifest`] acts as the "root of trust" for a database checkpoint. It catalogs
//! all other persisted components (vector indexes, string interner, etc.) and tracks metadata
//! like the Last Sequence Number (LSN) and creation timestamps.
//!
//! # Format
//!
//! The manifest is serialized using Bitcode and protected by a CRC32 checksum.
//!
//! ```text
//! [ Bitcode Encoded Manifest ] [ CRC32 (4 bytes) ]
//! ```
//!
//! # Atomic Persistence
//!
//! The manifest is saved using a "write-temp-then-rename" strategy. This ensures that the
//! `manifest.idx` file is updated atomically. If a crash occurs during writing, the original
//! manifest remains untouched, preventing database corruption.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crc32fast::Hasher;

use super::error::{IndexPersistenceError, Result};
use super::formats::IndexManifest;
use super::{MANIFEST_MAGIC, MANIFEST_VERSION};

impl IndexManifest {
    /// Create a new empty manifest.
    ///
    /// # Examples
    ///
    /// ```
    /// use aletheiadb::storage::index_persistence::formats::IndexManifest;
    ///
    /// let lsn = 12345;
    /// let manifest = IndexManifest::new(lsn);
    ///
    /// assert_eq!(manifest.lsn, lsn);
    /// assert!(manifest.vector_indexes.is_empty());
    /// ```
    pub fn new(lsn: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs() as i64;

        Self {
            magic: MANIFEST_MAGIC,
            version: MANIFEST_VERSION,
            created_at: now,
            last_modified: now,
            lsn,
            vector_indexes: Vec::new(),
            graph_index: None,
            temporal_index: None,
            temporal_adjacency_index: None,
            string_interner: None,
        }
    }

    /// Update the last_modified timestamp.
    pub fn touch(&mut self) {
        self.last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::from_secs(0))
            .as_secs() as i64;
    }

    /// Update the LSN.
    pub fn set_lsn(&mut self, lsn: u64) {
        self.lsn = lsn;
        self.touch();
    }
}

/// Save manifest to disk with CRC32 checksum using atomic write.
///
/// Format: `[bitcode_data][crc32_checksum_4_bytes]`
///
/// Uses write-temp-then-rename to prevent corruption on crash.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::index_persistence::manifest::save_manifest;
/// use aletheiadb::storage::index_persistence::formats::IndexManifest;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let path = dir.path().join("manifest.idx");
/// let manifest = IndexManifest::new(100);
///
/// save_manifest(&manifest, &path).unwrap();
/// assert!(path.exists());
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - The file cannot be written (e.g., permission denied, disk full).
/// - The atomic rename operation fails.
pub fn save_manifest(manifest: &IndexManifest, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(manifest);

    // Calculate CRC32 of the encoded data
    let mut hasher = Hasher::new();
    hasher.update(&encoded);
    let checksum = hasher.finalize();

    // Write data + checksum
    let mut data_with_checksum = encoded;
    data_with_checksum.extend_from_slice(&checksum.to_le_bytes());

    super::atomic_write(path, &data_with_checksum)?;
    Ok(())
}

/// Load manifest from disk and validate CRC32 checksum.
///
/// # Examples
///
/// ```
/// use aletheiadb::storage::index_persistence::manifest::{save_manifest, load_manifest};
/// use aletheiadb::storage::index_persistence::formats::IndexManifest;
/// use tempfile::tempdir;
///
/// let dir = tempdir().unwrap();
/// let path = dir.path().join("manifest.idx");
/// let original = IndexManifest::new(100);
///
/// save_manifest(&original, &path).unwrap();
///
/// let loaded = load_manifest(&path).unwrap();
/// assert_eq!(loaded.lsn, 100);
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - The file does not exist or cannot be read.
/// - The file size exceeds `MAX_MANIFEST_FILE_SIZE`.
/// - The CRC32 checksum verification fails (indicating corruption).
/// - The file has invalid magic bytes or an unsupported version.
pub fn load_manifest(path: &Path) -> Result<IndexManifest> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > super::MAX_MANIFEST_FILE_SIZE {
        return Err(IndexPersistenceError::SizeLimitExceeded {
            message: format!(
                "Manifest file size {} exceeds limit {}",
                metadata.len(),
                super::MAX_MANIFEST_FILE_SIZE
            ),
        });
    }

    let bytes = fs::read(path)?;

    // Check minimum size (must have at least 4 bytes for CRC)
    if bytes.len() < 4 {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "File too small to contain CRC32 checksum".into(),
        });
    }

    // Split data and checksum
    let (data, checksum_bytes) = bytes.split_at(bytes.len() - 4);
    let stored_checksum = u32::from_le_bytes(checksum_bytes.try_into().map_err(|_| {
        IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: "Invalid CRC32 checksum format".into(),
        }
    })?);

    // Verify checksum
    let mut hasher = Hasher::new();
    hasher.update(data);
    let computed_checksum = hasher.finalize();

    if computed_checksum != stored_checksum {
        return Err(IndexPersistenceError::Corrupted {
            path: path.to_path_buf(),
            source: format!(
                "CRC32 checksum mismatch: expected {}, got {}",
                stored_checksum, computed_checksum
            )
            .into(),
        });
    }

    // Decode and validate
    let manifest: IndexManifest = bitcode::decode(data)?;

    // Validate magic bytes
    if manifest.magic != MANIFEST_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: MANIFEST_MAGIC,
            got: manifest.magic,
        });
    }

    // Validate version
    if manifest.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: manifest.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        let mut manifest = IndexManifest::new(42);
        manifest.string_interner = Some(StringInternerManifestEntry {
            interner_file: "strings/interner.idx".to_string(),
            string_count: 100,
        });
        manifest.vector_indexes.push(VectorIndexManifestEntry {
            property_name: "embedding".to_string(),
            dimensions: 384,
            metric: 0,
            current_file: "vector/embedding/current.usearch".to_string(),
            mappings_file: "vector/embedding/current.mappings".to_string(),
            snapshot_count: 5,
            temporal_enabled: true,
        });

        save_manifest(&manifest, &path).unwrap();
        let loaded = load_manifest(&path).unwrap();

        assert_eq!(loaded.magic, MANIFEST_MAGIC);
        assert_eq!(loaded.lsn, 42);
        assert_eq!(loaded.vector_indexes.len(), 1);
        assert_eq!(loaded.vector_indexes[0].property_name, "embedding");
        assert!(loaded.string_interner.is_some());
    }

    #[test]
    fn test_manifest_touch_updates_timestamp() {
        let mut manifest = IndexManifest::new(0);
        let original = manifest.last_modified;

        std::thread::sleep(std::time::Duration::from_millis(10));
        manifest.touch();

        assert!(manifest.last_modified >= original);
    }

    #[test]
    fn test_manifest_crc_corruption_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        // Save a valid manifest
        let manifest = IndexManifest::new(42);
        save_manifest(&manifest, &path).unwrap();

        // Corrupt the data (change a byte in the middle)
        let mut bytes = fs::read(&path).unwrap();
        bytes[10] ^= 0xFF; // Flip all bits in one byte
        fs::write(&path, bytes).unwrap();

        // Loading should fail with corruption error
        let result = load_manifest(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }

    #[test]
    fn test_manifest_truncated_file_detected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        // Write a file that's too small
        fs::write(&path, b"ab").unwrap();

        let result = load_manifest(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Index file corrupted"));
    }
}

#[cfg(test)]
mod sentry_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_save_manifest_fails_on_io_error() {
        let dir = tempdir().unwrap();
        // Path inside non-existent subdirectory (should fail creation of temp file if atomic_write doesn't mkdir - which it doesn't)
        let path = dir.path().join("subdir").join("manifest.idx");

        let manifest = IndexManifest::new(1);
        let result = save_manifest(&manifest, &path);

        assert!(result.is_err());
        // Should be I/O error (NotFound because directory doesn't exist)
        assert!(result.unwrap_err().is_not_found());
    }

    #[test]
    fn test_load_manifest_invalid_magic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        let mut manifest = IndexManifest::new(1);
        // Corrupt magic bytes
        manifest.magic = *b"BADM";

        // Manually save because save_manifest uses correct magic in new() if we just used constructor?
        // Wait, IndexManifest::new() sets correct magic. We changed it.
        // But bitcode::encode encodes the struct as is. So we can use save_manifest if we could change magic.
        // manifest.magic is public, so we changed it.
        // BUT save_manifest calculates CRC. So CRC will be valid for the BAD MAGIC.
        // This tests that load_manifest checks magic *after* CRC validation.

        save_manifest(&manifest, &path).unwrap();

        let result = load_manifest(&path);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::InvalidMagic { .. }
        ));
    }

    #[test]
    fn test_load_manifest_unsupported_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        let mut manifest = IndexManifest::new(1);
        manifest.version = MANIFEST_VERSION + 1;

        save_manifest(&manifest, &path).unwrap();

        let result = load_manifest(&path);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::UnsupportedVersion { .. }
        ));
    }

    #[test]
    fn test_manifest_crc_covers_all_data() {
        // Ensure CRC calculation includes all fields
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.idx");

        let mut manifest = IndexManifest::new(1);
        manifest.lsn = 100;
        save_manifest(&manifest, &path).unwrap();

        // Read file
        let mut bytes = fs::read(&path).unwrap();
        // Modify LSN in the serialized data
        // LSN is near the beginning. Bitcode is variable length, but we can just flip bytes.
        // Flipping ANY byte in the data section (except last 4 CRC bytes) should trigger checksum mismatch.
        let len = bytes.len();
        bytes[len - 5] ^= 0xFF; // Flip last byte of data

        fs::write(&path, bytes).unwrap();

        let result = load_manifest(&path);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexPersistenceError::Corrupted { .. }
        ));
    }
}
