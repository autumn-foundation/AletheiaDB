//! Comprehensive index persistence layer for GallifreyDB.
//!
//! This module provides persistence for all index types:
//! - Vector indexes (HNSW via usearch)
//! - Graph indexes (CSR adjacency)
//! - Temporal indexes (version chains)
//! - String interner
//!
//! # Architecture
//!
//! ```text
//! indexes/
//! ├── manifest.idx          # Index registry
//! ├── strings/interner.idx  # String interning table
//! ├── graph/adjacency.idx   # CSR adjacency data
//! ├── temporal/versions.idx # Version chains
//! └── vector/{prop}/        # Per-property vector indexes
//! ```
//!
//! # Load Order
//!
//! 1. String interner (others depend on string indices)
//! 2. Manifest (tells us what indexes exist)
//! 3. Graph, Temporal, Vector (parallel)
//!
//! # Usage
//!
//! ```ignore
//! use gallifreydb::storage::index_persistence::{
//!     IndexPersistenceManager, PersistenceConfig, IndexManifest
//! };
//!
//! let manager = IndexPersistenceManager::new("data");
//! manager.ensure_directories()?;
//!
//! // Save
//! manager.save_string_interner()?;
//! let manifest = IndexManifest::new(100);
//! manager.save_manifest(&manifest)?;
//!
//! // Load (respects load order)
//! if manager.indexes_exist() {
//!     let manifest = manager.load_manifest_and_strings()?;
//!     // String interner is now restored
//!     // Ready to load other indexes
//! }
//! ```
//!
//! # Format Details
//!
//! All index files (except the native HNSW index) use [bitcode](https://github.com/llogiq/bitcode)
//! serialization with a consistent header format: `[MAGIC:4][VERSION:1][DATA...]`.
//!
//! | File Type | Magic Bytes | Rust Struct | Description |
//! |-----------|-------------|-------------|-------------|
//! | **Manifest** | `GIDX` | [`formats::IndexManifest`] | Registry of all indexes, LSN tracking, and timestamps. |
//! | **String Interner** | `GSTR` | [`formats::StringInternerData`] | Ordered list of interned strings for ID restoration. |
//! | **Graph Index** | `GGRP` | [`formats::GraphIndexData`] | Nodes, edges, CSR adjacency, and current properties. |
//! | **Graph Delta** | `GDLT` | [`formats::GraphIndexDelta`] | Incremental changes (added/modified/deleted) since base snapshot. |
//! | **Temporal Index** | `GTMP` | [`formats::TemporalIndexData`] | Version chains, anchors, and deltas for time-travel. |
//! | **Vector Meta** | `GVEC` | [`formats::VectorIndexMeta`] | Metadata for a vector index (dimensions, metric, etc.). |
//!
//! ## Vector Index Hybrid Format
//!
//! Vector indexes use a hybrid format for performance:
//! 1. **Metadata** (`meta.idx`): Bitcode-serialized [`formats::VectorIndexMeta`].
//! 2. **Mappings** (`mappings.idx`): Bitcode-serialized [`formats::VectorMappingsData`] mapping NodeIDs to usearch keys.
//! 3. **Index** (`current.usearch`): Native binary format produced by the `usearch` C++ library (HNSW graph).
//!
//! # Safety and Integrity
//!
//! - **Atomic Writes**: All files are written using a write-temp-then-rename strategy (`atomic_write`) to prevent corruption during crashes.
//! - **Checksums**: Many formats (Graph, Vector Meta/Mappings) include CRC32 checksums for integrity verification.
//! - **Magic Bytes**: All files start with 4 magic bytes to prevent parsing invalid file types.
//! - **Versioning**: All files include a version byte to support future schema evolution.

pub mod api;
mod error;
pub mod formats;
pub mod graph;
pub mod loader;
pub mod manifest;
/// Persistence operations implementation.
pub mod operations;
pub mod strings;
pub mod temporal;
/// Persistence mutation tracking.
pub mod tracker;
pub mod vector;
/// Background persistence worker thread.
pub mod worker;

#[cfg(test)]
mod dos_tests;

pub use api::{
    IndexStatus, PersistenceConfig, PersistenceStats, PersistenceStatus, VectorIndexStatus,
};
pub use error::{IndexPersistenceError, Result};
pub use formats::*;
pub use loader::IndexPersistenceManager;
use rayon::prelude::*;

/// Current manifest format version.
pub const MANIFEST_VERSION: u16 = 1;

/// Magic bytes for manifest files.
pub const MANIFEST_MAGIC: [u8; 4] = *b"GIDX";

/// Magic bytes for string interner files.
pub const INTERNER_MAGIC: [u8; 4] = *b"GSTR";

/// Magic bytes for graph index files.
pub const GRAPH_MAGIC: [u8; 4] = *b"GGRP";

/// Magic bytes for graph delta files.
pub const DELTA_MAGIC: [u8; 4] = *b"GDLT";

/// Magic bytes for temporal index files.
pub const TEMPORAL_MAGIC: [u8; 4] = *b"GTMP";

/// Magic bytes for vector metadata files.
pub const VECTOR_META_MAGIC: [u8; 4] = *b"GVEC";

/// Maximum number of strings allowed in the string interner (DoS protection).
/// ~100K strings should be sufficient for most databases while preventing
/// memory exhaustion attacks.
pub const MAX_STRING_COUNT: u64 = 100_000;

/// Maximum length of a single string in bytes (DoS protection).
/// 1MB per string is generous while preventing memory exhaustion.
pub const MAX_STRING_LENGTH: usize = 1_048_576; // 1MB

/// Maximum vector dimension (DoS protection).
/// 100K dimensions aligns with the documented maximum.
/// At 4 bytes per f32, this is 400KB per vector.
pub const MAX_VECTOR_DIMENSIONS: usize = 100_000;

/// Maximum size of a graph index file (DoS protection).
///
/// Limits the amount of memory allocated when loading graph indexes.
/// Default: 4GB.
#[cfg(not(test))]
pub const MAX_GRAPH_INDEX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024; // 4GB

/// Maximum size of a graph index file (DoS protection).
///
/// Limits the amount of memory allocated when loading graph indexes.
/// Default: 10MB (test mode).
#[cfg(test)]
pub const MAX_GRAPH_INDEX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB

/// Maximum size of a vector index metadata/mappings file (DoS protection).
///
/// Limits the amount of memory allocated when loading vector index metadata.
/// Default: 1GB.
#[cfg(not(test))]
pub const MAX_VECTOR_INDEX_FILE_SIZE: u64 = 1024 * 1024 * 1024; // 1GB

/// Maximum size of a vector index metadata/mappings file (DoS protection).
///
/// Limits the amount of memory allocated when loading vector index metadata.
/// Default: 5MB (test mode).
#[cfg(test)]
pub const MAX_VECTOR_INDEX_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5MB

/// Maximum size of a temporal index file (DoS protection).
///
/// Limits the amount of memory allocated when loading temporal indexes.
/// Default: 2GB.
#[cfg(not(test))]
pub const MAX_TEMPORAL_INDEX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB

/// Maximum size of a temporal index file (DoS protection).
///
/// Limits the amount of memory allocated when loading temporal indexes.
/// Default: 10MB (test mode).
#[cfg(test)]
pub const MAX_TEMPORAL_INDEX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB

/// Maximum size of a string interner file (DoS protection).
///
/// Limits the amount of memory allocated when loading the string interner.
/// Default: 256MB.
#[cfg(not(test))]
pub const MAX_STRING_INTERNER_FILE_SIZE: u64 = 256 * 1024 * 1024; // 256MB

/// Maximum size of a string interner file (DoS protection).
///
/// Limits the amount of memory allocated when loading the string interner.
/// Default: 5MB (test mode).
#[cfg(test)]
pub const MAX_STRING_INTERNER_FILE_SIZE: u64 = 5 * 1024 * 1024; // 5MB

/// Maximum size of a manifest file (DoS protection).
///
/// Limits the amount of memory allocated when loading the manifest.
/// Default: 1MB.
#[cfg(not(test))]
pub const MAX_MANIFEST_FILE_SIZE: u64 = 1024 * 1024; // 1MB

/// Maximum size of a manifest file (DoS protection).
///
/// Limits the amount of memory allocated when loading the manifest.
/// Default: 100KB (test mode).
#[cfg(test)]
pub const MAX_MANIFEST_FILE_SIZE: u64 = 100 * 1024; // 100KB

/// Maximum allowed file size for memory-mapped files (Sanity Check).
///
/// Prevents attempting to map ridiculously large or sparse files that could cause issues.
/// Default: 100GB.
#[cfg(not(test))]
pub const MAX_MMAP_FILE_SIZE: u64 = 100 * 1024 * 1024 * 1024; // 100GB

/// Maximum allowed file size for memory-mapped files (Sanity Check).
///
/// Prevents attempting to map ridiculously large or sparse files that could cause issues.
/// Default: 100MB (test mode).
#[cfg(test)]
pub const MAX_MMAP_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100MB

/// Atomically write data to a file using write-temp-then-rename pattern.
///
/// This prevents corruption if the process crashes mid-write:
/// 1. Write to `{path}.tmp`
/// 2. Sync to disk
/// 3. Rename temp → target (atomic on POSIX, nearly-atomic on Windows)
///
/// # Errors
///
/// Returns an error if:
/// - Failed to write temp file
/// - Failed to sync to disk
/// - Failed to rename temp to target
pub(crate) fn atomic_write(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::fs;
    use std::io::Write;

    // Write to temporary file
    let temp_path = path.with_extension("tmp");
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(data)?;
    file.sync_all()?; // Ensure data is on disk

    // Atomically replace target with temp
    fs::rename(&temp_path, path)?;

    Ok(())
}

/// Load graph, temporal, and vector indexes in parallel for faster startup.
///
/// This function spawns threads to load all three index types concurrently,
/// reducing startup time for databases with large indexes.
///
/// # Arguments
///
/// * `graph_path` - Path to the graph index file
/// * `temporal_path` - Optional path to the temporal index file
/// * `vector_paths` - Optional vector of vector index paths (meta, mappings, snapshots)
///
/// # Returns
///
/// A tuple of (graph_data, temporal_data_option, vector_data_vec)
///
/// # Errors
///
/// Returns an error if any of the index files fail to load.
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::storage::index_persistence::load_indexes_parallel;
///
/// let (graph, temporal, vector) = load_indexes_parallel(
///     &graph_path,
///     Some(&temporal_path),
///     vec![],
/// )?;
/// ```
pub fn load_indexes_parallel(
    graph_path: &std::path::Path,
    temporal_path: Option<&std::path::Path>,
    vector_paths: Vec<&std::path::Path>,
) -> Result<(
    formats::GraphIndexData,
    Option<formats::TemporalIndexData>,
    Vec<formats::VectorIndexData>,
)> {
    use std::thread;

    // Convert paths to owned PathBufs for thread safety
    let graph_path = graph_path.to_path_buf();
    let temporal_path_opt = temporal_path.map(|p| p.to_path_buf());
    let vector_paths: Vec<_> = vector_paths.into_iter().map(|p| p.to_path_buf()).collect();

    // Spawn thread for graph loading
    let graph_handle = thread::spawn(move || graph::load_graph_index(&graph_path));

    // Spawn thread for temporal loading if path provided
    let temporal_handle =
        temporal_path_opt.map(|path| thread::spawn(move || temporal::load_temporal_index(&path)));

    // Load vector indexes in parallel using Rayon (blocks current thread)
    // We do this while graph and temporal indexes are loading in background threads
    let vector_data: Result<Vec<_>> = vector_paths
        .par_iter()
        .map(|path| vector::load_vector_index(path))
        .collect();
    let vector_data = vector_data?;

    // Join threads and collect results
    let graph_data = graph_handle
        .join()
        .expect("Graph loading thread panicked")?;

    let temporal_data = if let Some(handle) = temporal_handle {
        Some(handle.join().expect("Temporal loading thread panicked")?)
    } else {
        None
    };

    Ok((graph_data, temporal_data, vector_data))
}
