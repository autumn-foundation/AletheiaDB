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
