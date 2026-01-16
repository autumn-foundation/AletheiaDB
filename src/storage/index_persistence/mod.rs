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
//! ├── temporal/*.idx        # Version chains
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
//!     IndexPersistenceManager, PersistenceConfig
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
//! All index files use bitcode serialization with magic bytes and version validation:
//!
//! - **Manifest** (`GIDX`): Index registry, LSN tracking, timestamps
//! - **String Interner** (`GSTR`): Ordered string list for ID restoration
//! - **Graph** (`GGRP`): Nodes, edges, CSR adjacency, properties
//! - **Temporal** (`GTMP`): Version chains, anchors, deltas
//! - **Vector** (`GVEC`): Metadata, ID mappings, HNSW snapshots
//!
//! # Versioning
//!
//! Current format version: 1
//!
//! Backward compatibility:
//! - Same major version: guaranteed compatible
//! - Newer minor version: older code can read newer files
//! - Unsupported version: returns `UnsupportedVersion` error

pub mod api;
mod error;
pub mod formats;
pub mod graph;
pub mod loader;
pub mod manifest;
pub mod strings;
pub mod temporal;
pub mod vector;

pub use api::{
    IndexStatus, PersistenceConfig, PersistenceStats, PersistenceStatus, VectorIndexStatus,
};
pub use error::{IndexPersistenceError, Result};
pub use formats::*;
pub use loader::IndexPersistenceManager;

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
    _vector_paths: Vec<&std::path::Path>, // TODO: implement vector loading
) -> Result<(formats::GraphIndexData, Option<formats::TemporalIndexData>)> {
    use std::thread;

    // Convert paths to owned PathBufs for thread safety
    let graph_path = graph_path.to_path_buf();
    let temporal_path_opt = temporal_path.map(|p| p.to_path_buf());

    // Spawn thread for graph loading
    let graph_handle = thread::spawn(move || graph::load_graph_index(&graph_path));

    // Spawn thread for temporal loading if path provided
    let temporal_handle =
        temporal_path_opt.map(|path| thread::spawn(move || temporal::load_temporal_index(&path)));

    // TODO: Spawn threads for vector index loading

    // Join threads and collect results
    let graph_data = graph_handle
        .join()
        .expect("Graph loading thread panicked")?;

    let temporal_data = if let Some(handle) = temporal_handle {
        Some(handle.join().expect("Temporal loading thread panicked")?)
    } else {
        None
    };

    Ok((graph_data, temporal_data))
}
