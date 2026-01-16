# Index Persistence Layer Implementation Plan

**STATUS: ✅ COMPLETED (2026-01-15)**

> **Note:** This document is now historical. All tasks have been implemented. See [ADR-0023](../adr/0023-index-persistence-layer.md) for the final architecture.

**Goal:** Implement comprehensive index persistence for all index types (vector, graph, temporal, strings) with bitcode serialization and memory-mapped loading.

**Architecture:** Modular persistence layer in `src/storage/index_persistence/` with bitcode-serialized formats. Each index type has its own persistence module with index-specific triggers. String interner loads first (dependency), then manifest, then parallel load of remaining indexes. Memory-mapped loading with copy-on-write for mutations.

**Tech Stack:** bitcode (serialization), memmap2 (memory-mapping), usearch (vector index native format), rayon (parallel loading)

**Design Doc:** `docs/plans/2026-01-15-index-persistence-design.md`
**ADR:** `docs/adr/0023-index-persistence-layer.md`

---

## Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add bitcode and memmap2 dependencies**

Add to `[dependencies]` section in `Cargo.toml`:

```toml
bitcode = "0.6"
memmap2 = "0.9"
```

Verify `tempfile` is already in `[dev-dependencies]` (should be).

**Step 2: Verify dependencies resolve**

Run: `cargo check`
Expected: Compiles successfully

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add bitcode and memmap2 dependencies for index persistence"
```

---

## Task 2: Create Module Structure

**Files:**
- Create: `src/storage/index_persistence/mod.rs`
- Create: `src/storage/index_persistence/error.rs`
- Modify: `src/storage/mod.rs`

**Step 1: Create error types**

Create `src/storage/index_persistence/error.rs`:

```rust
//! Error types for index persistence operations.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during index persistence operations.
#[derive(Debug, Error)]
pub enum IndexPersistenceError {
    /// Index file is corrupted or invalid
    #[error("Index file corrupted: {path}")]
    Corrupted {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// String interner index mismatch during restoration
    #[error("String interner mismatch: expected index {expected}, got {got}")]
    InternerMismatch { expected: u32, got: u32 },

    /// Manifest version not supported
    #[error("Manifest version {found} not supported (max supported: {supported})")]
    UnsupportedVersion { found: u16, supported: u16 },

    /// Required index file is missing
    #[error("Missing required index file: {name}")]
    MissingIndex { name: String },

    /// Invalid magic bytes in file header
    #[error("Invalid magic bytes in {path}: expected {expected:?}, got {got:?}")]
    InvalidMagic {
        path: PathBuf,
        expected: [u8; 4],
        got: [u8; 4],
    },

    /// IO error during persistence operations
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Bitcode serialization/deserialization error
    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<bitcode::Error> for IndexPersistenceError {
    fn from(e: bitcode::Error) -> Self {
        IndexPersistenceError::Serialization(e.to_string())
    }
}

/// Result type for index persistence operations.
pub type Result<T> = std::result::Result<T, IndexPersistenceError>;
```

**Step 2: Create module entry point**

Create `src/storage/index_persistence/mod.rs`:

```rust
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

mod error;

pub use error::{IndexPersistenceError, Result};

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
```

**Step 3: Export from storage module**

Add to `src/storage/mod.rs`:

```rust
pub mod index_persistence;
```

**Step 4: Verify compiles**

Run: `cargo check`
Expected: Compiles successfully

**Step 5: Commit**

```bash
git add src/storage/index_persistence/ src/storage/mod.rs
git commit -m "feat: add index persistence module structure and error types"
```

---

## Task 3: Create Bitcode Format Structs

**Files:**
- Create: `src/storage/index_persistence/formats.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Create format structs**

Create `src/storage/index_persistence/formats.rs`:

```rust
//! Bitcode-serializable format structs for index persistence.

use bitcode::{Decode, Encode};

// ============================================================================
// Manifest Formats
// ============================================================================

/// Root manifest - entry point for index loading.
#[derive(Debug, Clone, Encode, Decode)]
pub struct IndexManifest {
    /// Magic bytes: "GIDX"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,
    /// Unix timestamp when created
    pub created_at: i64,
    /// Unix timestamp of last modification
    pub last_modified: i64,
    /// LSN this manifest is consistent with
    pub lsn: u64,

    /// Vector index entries (one per property)
    pub vector_indexes: Vec<VectorIndexManifestEntry>,
    /// Graph index entry
    pub graph_index: Option<GraphIndexManifestEntry>,
    /// Temporal index entry
    pub temporal_index: Option<TemporalIndexManifestEntry>,
    /// String interner entry
    pub string_interner: Option<StringInternerManifestEntry>,
}

/// Manifest entry for a vector index.
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorIndexManifestEntry {
    /// Property name this index covers
    pub property_name: String,
    /// Vector dimensions
    pub dimensions: u32,
    /// Distance metric (0=Cosine, 1=Euclidean, 2=DotProduct)
    pub metric: u8,
    /// Relative path to current index file
    pub current_file: String,
    /// Relative path to mappings file
    pub mappings_file: String,
    /// Number of temporal snapshots
    pub snapshot_count: u32,
    /// Whether temporal indexing is enabled
    pub temporal_enabled: bool,
}

/// Manifest entry for graph index.
#[derive(Debug, Clone, Encode, Decode)]
pub struct GraphIndexManifestEntry {
    /// Relative path to adjacency file
    pub adjacency_file: String,
    /// Number of nodes
    pub node_count: u64,
    /// Number of edges
    pub edge_count: u64,
}

/// Manifest entry for temporal index.
#[derive(Debug, Clone, Encode, Decode)]
pub struct TemporalIndexManifestEntry {
    /// Relative path to node versions file
    pub node_versions_file: String,
    /// Relative path to edge versions file
    pub edge_versions_file: String,
    /// Total version count
    pub version_count: u64,
}

/// Manifest entry for string interner.
#[derive(Debug, Clone, Encode, Decode)]
pub struct StringInternerManifestEntry {
    /// Relative path to interner file
    pub interner_file: String,
    /// Number of interned strings
    pub string_count: u64,
}

// ============================================================================
// String Interner Format
// ============================================================================

/// Persisted string interner data.
#[derive(Debug, Clone, Encode, Decode)]
pub struct StringInternerData {
    /// Magic bytes: "GSTR"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,
    /// Number of strings
    pub string_count: u64,
    /// Strings in index order (index 0 = first string)
    pub strings: Vec<String>,
}

// ============================================================================
// Graph Index Format
// ============================================================================

/// Persisted graph index data.
#[derive(Debug, Clone, Encode, Decode)]
pub struct GraphIndexData {
    /// Magic bytes: "GGRP"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,
    /// Number of nodes
    pub node_count: u64,
    /// Number of edges
    pub edge_count: u64,

    /// Node data
    pub nodes: Vec<PersistedNode>,
    /// Edge data
    pub edges: Vec<PersistedEdge>,

    /// CSR outgoing adjacency: offsets into neighbors array
    pub outgoing_offsets: Vec<u64>,
    /// CSR outgoing adjacency: packed edge IDs
    pub outgoing_neighbors: Vec<u64>,

    /// CSR incoming adjacency: offsets into neighbors array
    pub incoming_offsets: Vec<u64>,
    /// CSR incoming adjacency: packed edge IDs
    pub incoming_neighbors: Vec<u64>,
}

/// Persisted node data.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistedNode {
    /// Node ID
    pub id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Node properties
    pub properties: PersistedPropertyMap,
}

/// Persisted edge data.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistedEdge {
    /// Edge ID
    pub id: u64,
    /// Source node ID
    pub source_id: u64,
    /// Target node ID
    pub target_id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Edge properties
    pub properties: PersistedPropertyMap,
}

/// Persisted property map.
#[derive(Debug, Clone, Default, Encode, Decode)]
pub struct PersistedPropertyMap {
    /// Property entries: (key_index, value)
    pub entries: Vec<(u32, PersistedPropertyValue)>,
}

/// Persisted property value.
#[derive(Debug, Clone, Encode, Decode)]
pub enum PersistedPropertyValue {
    /// Null value
    Null,
    /// Boolean value
    Bool(bool),
    /// Integer value
    Int(i64),
    /// Float value
    Float(f64),
    /// String index in interner
    String(u32),
    /// Raw bytes
    Bytes(Vec<u8>),
    /// Vector embedding
    Vector(Vec<f32>),
    /// Array of values
    Array(Vec<PersistedPropertyValue>),
    /// Map of values
    Map(Vec<(u32, PersistedPropertyValue)>),
}

// ============================================================================
// Temporal Index Format
// ============================================================================

/// Persisted temporal index data.
#[derive(Debug, Clone, Encode, Decode)]
pub struct TemporalIndexData {
    /// Magic bytes: "GTMP"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,

    /// Node version entries
    pub node_versions: Vec<NodeVersionEntry>,
    /// Node anchor entries
    pub node_anchors: Vec<NodeAnchorEntry>,

    /// Edge version entries
    pub edge_versions: Vec<EdgeVersionEntry>,
    /// Edge anchor entries
    pub edge_anchors: Vec<EdgeAnchorEntry>,
}

/// Persisted node version entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct NodeVersionEntry {
    /// Node ID
    pub node_id: u64,
    /// Valid time start (unix timestamp)
    pub valid_from: i64,
    /// Valid time end (None = still valid)
    pub valid_to: Option<i64>,
    /// Transaction time (unix timestamp)
    pub tx_time: i64,
    /// Version type (delta or anchor)
    pub version_type: PersistedVersionType,
    /// Properties at this version
    pub properties: PersistedPropertyMap,
    /// Vector snapshot ID for provenance tracking
    pub vector_snapshot_id: Option<u64>,
}

/// Persisted node anchor entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct NodeAnchorEntry {
    /// Node ID
    pub node_id: u64,
    /// Anchor transaction time
    pub anchor_tx_time: i64,
    /// Full state snapshot
    pub full_state: PersistedPropertyMap,
    /// Vector snapshot ID
    pub vector_snapshot_id: Option<u64>,
}

/// Persisted version type.
#[derive(Debug, Clone, Encode, Decode)]
pub enum PersistedVersionType {
    /// Delta referencing a base anchor
    Delta {
        /// Transaction time of base anchor
        base_anchor_tx: i64,
    },
    /// Full anchor snapshot
    Anchor,
}

/// Persisted edge version entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct EdgeVersionEntry {
    /// Edge ID
    pub edge_id: u64,
    /// Source node ID
    pub source_id: u64,
    /// Target node ID
    pub target_id: u64,
    /// Valid time start
    pub valid_from: i64,
    /// Valid time end
    pub valid_to: Option<i64>,
    /// Transaction time
    pub tx_time: i64,
    /// Version type
    pub version_type: PersistedVersionType,
    /// Properties
    pub properties: PersistedPropertyMap,
}

/// Persisted edge anchor entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct EdgeAnchorEntry {
    /// Edge ID
    pub edge_id: u64,
    /// Anchor transaction time
    pub anchor_tx_time: i64,
    /// Full state snapshot
    pub full_state: PersistedPropertyMap,
}

// ============================================================================
// Vector Index Format
// ============================================================================

/// Vector index metadata.
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorIndexMeta {
    /// Magic bytes: "GVEC"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,
    /// Property name
    pub property_name: String,
    /// Vector dimensions
    pub dimensions: u32,
    /// Distance metric (0=Cosine, 1=Euclidean, 2=DotProduct)
    pub metric: u8,
    /// HNSW configuration
    pub hnsw_config: PersistedHnswConfig,
    /// Number of vectors
    pub vector_count: u64,
    /// Creation timestamp
    pub created_at: i64,
    /// Last modification timestamp
    pub last_modified: i64,
}

/// Persisted HNSW configuration.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistedHnswConfig {
    /// Max connections per node
    pub m: u16,
    /// Construction-time ef
    pub ef_construction: u16,
    /// Search-time ef
    pub ef_search: u16,
}

/// Vector ID mappings (NodeId <-> usearch key).
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorMappingsData {
    /// Format version
    pub version: u16,
    /// Number of mappings
    pub count: u64,
    /// ID mappings
    pub mappings: Vec<VectorMapping>,
    /// Soft-deleted node IDs
    pub deleted_ids: Vec<u64>,
}

/// Single vector ID mapping.
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorMapping {
    /// GallifreyDB node ID
    pub node_id: u64,
    /// usearch internal key
    pub usearch_key: u64,
}

/// Vector snapshot metadata.
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorSnapshotMeta {
    /// Snapshot ID
    pub snapshot_id: u64,
    /// Snapshot type (full or delta)
    pub snapshot_type: PersistedSnapshotType,
    /// Timestamp when created
    pub timestamp: i64,
    /// Number of vectors in snapshot
    pub vector_count: u64,
    /// HNSW config at snapshot time
    pub config: PersistedHnswConfig,
    /// Base snapshot ID (for delta snapshots)
    pub base_snapshot_id: Option<u64>,
}

/// Persisted snapshot type.
#[derive(Debug, Clone, Encode, Decode)]
pub enum PersistedSnapshotType {
    /// Full index snapshot
    Full,
    /// Delta snapshot with change count
    Delta {
        /// Number of changes from base
        changes_count: u64,
    },
}

// ============================================================================
// Persistence Policies
// ============================================================================

/// Persistence policies for all index types.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistencePolicies {
    /// Vector index persistence policy
    pub vector: VectorPersistencePolicy,
    /// Graph index persistence policy
    pub graph: GraphPersistencePolicy,
    /// Temporal index persistence policy
    pub temporal: TemporalPersistencePolicy,
    /// String interner persistence policy
    pub strings: StringPersistencePolicy,
}

impl Default for PersistencePolicies {
    fn default() -> Self {
        Self {
            vector: VectorPersistencePolicy::default(),
            graph: GraphPersistencePolicy::default(),
            temporal: TemporalPersistencePolicy::default(),
            strings: StringPersistencePolicy::default(),
        }
    }
}

/// Vector index persistence policy.
#[derive(Debug, Clone, Encode, Decode)]
pub struct VectorPersistencePolicy {
    /// Persist after N mutations
    pub mutation_threshold: u32,
    /// Persist after N seconds
    pub time_interval_secs: u32,
}

impl Default for VectorPersistencePolicy {
    fn default() -> Self {
        Self {
            mutation_threshold: 1000,
            time_interval_secs: 300, // 5 minutes
        }
    }
}

/// Graph index persistence policy.
#[derive(Debug, Clone, Encode, Decode)]
pub struct GraphPersistencePolicy {
    /// Persist after adjacency rebuild
    pub on_adjacency_rebuild: bool,
    /// Persist after N mutations
    pub mutation_threshold: u32,
    /// Persist after N seconds
    pub time_interval_secs: u32,
}

impl Default for GraphPersistencePolicy {
    fn default() -> Self {
        Self {
            on_adjacency_rebuild: true,
            mutation_threshold: 5000,
            time_interval_secs: 600, // 10 minutes
        }
    }
}

/// Temporal index persistence policy.
#[derive(Debug, Clone, Encode, Decode)]
pub struct TemporalPersistencePolicy {
    /// Persist after N new versions
    pub version_threshold: u32,
    /// Persist after N anchors
    pub anchor_threshold: u32,
    /// Persist after N seconds
    pub time_interval_secs: u32,
}

impl Default for TemporalPersistencePolicy {
    fn default() -> Self {
        Self {
            version_threshold: 1000,
            anchor_threshold: 100,
            time_interval_secs: 300, // 5 minutes
        }
    }
}

/// String interner persistence policy.
#[derive(Debug, Clone, Encode, Decode)]
pub struct StringPersistencePolicy {
    /// Persist after N new strings
    pub new_strings_threshold: u32,
    /// Persist after N seconds
    pub time_interval_secs: u32,
}

impl Default for StringPersistencePolicy {
    fn default() -> Self {
        Self {
            new_strings_threshold: 500,
            time_interval_secs: 600, // 10 minutes
        }
    }
}
```

**Step 2: Export formats from module**

Update `src/storage/index_persistence/mod.rs` to add:

```rust
pub mod formats;

pub use formats::*;
```

**Step 3: Verify compiles**

Run: `cargo check`
Expected: Compiles successfully

**Step 4: Run tests**

Run: `cargo test --lib`
Expected: All tests pass

**Step 5: Commit**

```bash
git add src/storage/index_persistence/formats.rs src/storage/index_persistence/mod.rs
git commit -m "feat: add bitcode format structs for index persistence"
```

---

## Task 4: Implement String Interner Persistence

**Files:**
- Create: `src/storage/index_persistence/strings.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Write failing test for round-trip serialization**

Create `src/storage/index_persistence/strings.rs`:

```rust
//! String interner persistence.

use crate::core::GLOBAL_INTERNER;
use std::fs;
use std::path::Path;

use super::error::{IndexPersistenceError, Result};
use super::formats::StringInternerData;
use super::{INTERNER_MAGIC, MANIFEST_VERSION};

/// Save the global string interner to disk.
pub fn save_string_interner(path: &Path) -> Result<()> {
    let strings = GLOBAL_INTERNER.get_all_strings();

    let data = StringInternerData {
        magic: INTERNER_MAGIC,
        version: MANIFEST_VERSION,
        string_count: strings.len() as u64,
        strings,
    };

    let encoded = bitcode::encode(&data)?;
    fs::write(path, encoded)?;

    Ok(())
}

/// Load the string interner from disk and restore GLOBAL_INTERNER.
pub fn load_string_interner(path: &Path) -> Result<StringInternerData> {
    let bytes = fs::read(path)?;
    let data: StringInternerData = bitcode::decode(&bytes)?;

    // Validate magic bytes
    if data.magic != INTERNER_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: INTERNER_MAGIC,
            got: data.magic,
        });
    }

    // Validate version
    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Restore GLOBAL_INTERNER from persisted data.
///
/// This must be called before loading any other indexes since they
/// reference string indices.
pub fn restore_string_interner(data: &StringInternerData) -> Result<()> {
    for (idx, s) in data.strings.iter().enumerate() {
        let interned_idx = GLOBAL_INTERNER.intern(s);
        // The interner should assign indices in order
        // If not, the interner had pre-existing strings which is a bug
        if interned_idx != idx as u32 {
            return Err(IndexPersistenceError::InternerMismatch {
                expected: idx as u32,
                got: interned_idx,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_string_interner_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interner.idx");

        // Intern some strings
        let idx1 = GLOBAL_INTERNER.intern("test_string_1");
        let idx2 = GLOBAL_INTERNER.intern("test_string_2");
        let idx3 = GLOBAL_INTERNER.intern("test_string_3");

        // Save
        save_string_interner(&path).unwrap();

        // Load
        let loaded = load_string_interner(&path).unwrap();

        assert_eq!(loaded.magic, INTERNER_MAGIC);
        assert!(loaded.strings.contains(&"test_string_1".to_string()));
        assert!(loaded.strings.contains(&"test_string_2".to_string()));
        assert!(loaded.strings.contains(&"test_string_3".to_string()));
    }

    #[test]
    fn test_invalid_magic_rejected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.idx");

        // Write garbage
        let bad_data = StringInternerData {
            magic: *b"BAAD",
            version: 1,
            string_count: 0,
            strings: vec![],
        };
        let encoded = bitcode::encode(&bad_data).unwrap();
        fs::write(&path, encoded).unwrap();

        // Should fail
        let result = load_string_interner(&path);
        assert!(matches!(result, Err(IndexPersistenceError::InvalidMagic { .. })));
    }
}
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod strings;
```

**Step 3: Run tests**

Run: `cargo test --lib string_interner_round_trip`
Expected: Test passes

Run: `cargo test --lib invalid_magic_rejected`
Expected: Test passes

**Step 4: Commit**

```bash
git add src/storage/index_persistence/strings.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement string interner persistence with bitcode"
```

---

## Task 5: Implement Manifest Persistence

**Files:**
- Create: `src/storage/index_persistence/manifest.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Implement manifest save/load**

Create `src/storage/index_persistence/manifest.rs`:

```rust
//! Index manifest persistence.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{IndexPersistenceError, Result};
use super::formats::IndexManifest;
use super::{MANIFEST_MAGIC, MANIFEST_VERSION};

impl IndexManifest {
    /// Create a new empty manifest.
    pub fn new(lsn: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
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
            string_interner: None,
        }
    }

    /// Update the last_modified timestamp.
    pub fn touch(&mut self) {
        self.last_modified = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
    }

    /// Update the LSN.
    pub fn set_lsn(&mut self, lsn: u64) {
        self.lsn = lsn;
        self.touch();
    }
}

/// Save manifest to disk.
pub fn save_manifest(manifest: &IndexManifest, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(manifest)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load manifest from disk.
pub fn load_manifest(path: &Path) -> Result<IndexManifest> {
    let bytes = fs::read(path)?;
    let manifest: IndexManifest = bitcode::decode(&bytes)?;

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
}
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod manifest;
```

**Step 3: Run tests**

Run: `cargo test --lib manifest_round_trip`
Expected: Test passes

**Step 4: Commit**

```bash
git add src/storage/index_persistence/manifest.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement manifest persistence with bitcode"
```

---

## Task 6: Implement Graph Index Persistence

**Files:**
- Create: `src/storage/index_persistence/graph.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Create graph persistence with conversion functions**

Create `src/storage/index_persistence/graph.rs`:

```rust
//! Graph index persistence.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::types::{Edge, Node, NodeId, EdgeId};
use crate::core::GLOBAL_INTERNER;

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    GraphIndexData, PersistedEdge, PersistedNode, PersistedPropertyMap, PersistedPropertyValue,
};
use super::{GRAPH_MAGIC, MANIFEST_VERSION};

/// Convert PropertyValue to PersistedPropertyValue.
pub fn persist_property_value(value: &PropertyValue) -> PersistedPropertyValue {
    match value {
        PropertyValue::Null => PersistedPropertyValue::Null,
        PropertyValue::Bool(b) => PersistedPropertyValue::Bool(*b),
        PropertyValue::Int(i) => PersistedPropertyValue::Int(*i),
        PropertyValue::Float(f) => PersistedPropertyValue::Float(*f),
        PropertyValue::String(s) => {
            let idx = GLOBAL_INTERNER.intern(s);
            PersistedPropertyValue::String(idx)
        }
        PropertyValue::Bytes(b) => PersistedPropertyValue::Bytes(b.to_vec()),
        PropertyValue::Vector(v) => PersistedPropertyValue::Vector(v.to_vec()),
        PropertyValue::Array(arr) => {
            PersistedPropertyValue::Array(arr.iter().map(persist_property_value).collect())
        }
        PropertyValue::Map(map) => {
            let entries: Vec<_> = map
                .iter()
                .map(|(k, v)| {
                    let key_idx = GLOBAL_INTERNER.intern(k);
                    (key_idx, persist_property_value(v))
                })
                .collect();
            PersistedPropertyValue::Map(entries)
        }
    }
}

/// Convert PersistedPropertyValue back to PropertyValue.
pub fn restore_property_value(persisted: &PersistedPropertyValue) -> PropertyValue {
    match persisted {
        PersistedPropertyValue::Null => PropertyValue::Null,
        PersistedPropertyValue::Bool(b) => PropertyValue::Bool(*b),
        PersistedPropertyValue::Int(i) => PropertyValue::Int(*i),
        PersistedPropertyValue::Float(f) => PropertyValue::Float(*f),
        PersistedPropertyValue::String(idx) => {
            let s = GLOBAL_INTERNER.resolve(*idx).unwrap_or_default();
            PropertyValue::String(s)
        }
        PersistedPropertyValue::Bytes(b) => PropertyValue::Bytes(Arc::from(b.as_slice())),
        PersistedPropertyValue::Vector(v) => PropertyValue::Vector(Arc::from(v.as_slice())),
        PersistedPropertyValue::Array(arr) => {
            PropertyValue::Array(Arc::from(arr.iter().map(restore_property_value).collect::<Vec<_>>()))
        }
        PersistedPropertyValue::Map(entries) => {
            let map: std::collections::HashMap<String, PropertyValue> = entries
                .iter()
                .map(|(key_idx, v)| {
                    let key = GLOBAL_INTERNER.resolve(*key_idx).unwrap_or_default();
                    (key, restore_property_value(v))
                })
                .collect();
            PropertyValue::Map(Arc::new(map))
        }
    }
}

/// Convert PropertyMap to PersistedPropertyMap.
pub fn persist_property_map(props: &PropertyMap) -> PersistedPropertyMap {
    let entries: Vec<_> = props
        .iter()
        .map(|(k, v)| {
            let key_idx = GLOBAL_INTERNER.intern(k);
            (key_idx, persist_property_value(v))
        })
        .collect();
    PersistedPropertyMap { entries }
}

/// Convert PersistedPropertyMap back to PropertyMap.
pub fn restore_property_map(persisted: &PersistedPropertyMap) -> PropertyMap {
    let mut map = PropertyMap::new();
    for (key_idx, value) in &persisted.entries {
        let key = GLOBAL_INTERNER.resolve(*key_idx).unwrap_or_default();
        map.insert(key, restore_property_value(value));
    }
    map
}

/// Save graph index data to disk.
pub fn save_graph_index(data: &GraphIndexData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load graph index data from disk.
pub fn load_graph_index(path: &Path) -> Result<GraphIndexData> {
    let bytes = fs::read(path)?;
    let data: GraphIndexData = bitcode::decode(&bytes)?;

    if data.magic != GRAPH_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: GRAPH_MAGIC,
            got: data.magic,
        });
    }

    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Create a new empty GraphIndexData.
pub fn new_graph_index_data() -> GraphIndexData {
    GraphIndexData {
        magic: GRAPH_MAGIC,
        version: MANIFEST_VERSION,
        node_count: 0,
        edge_count: 0,
        nodes: Vec::new(),
        edges: Vec::new(),
        outgoing_offsets: Vec::new(),
        outgoing_neighbors: Vec::new(),
        incoming_offsets: Vec::new(),
        incoming_neighbors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_property_value_round_trip() {
        // Test various property types
        let values = vec![
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Int(42),
            PropertyValue::Float(3.14),
            PropertyValue::String("test".to_string()),
            PropertyValue::Bytes(Arc::from(vec![1u8, 2, 3].as_slice())),
            PropertyValue::Vector(Arc::from(vec![1.0f32, 2.0, 3.0].as_slice())),
        ];

        for value in values {
            let persisted = persist_property_value(&value);
            let restored = restore_property_value(&persisted);

            // Compare string representation for simplicity
            assert_eq!(format!("{:?}", value), format!("{:?}", restored));
        }
    }

    #[test]
    fn test_graph_index_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("graph.idx");

        let mut data = new_graph_index_data();
        data.node_count = 2;
        data.nodes.push(PersistedNode {
            id: 1,
            label_idx: GLOBAL_INTERNER.intern("Person"),
            properties: PersistedPropertyMap { entries: vec![] },
        });
        data.nodes.push(PersistedNode {
            id: 2,
            label_idx: GLOBAL_INTERNER.intern("Document"),
            properties: PersistedPropertyMap { entries: vec![] },
        });

        save_graph_index(&data, &path).unwrap();
        let loaded = load_graph_index(&path).unwrap();

        assert_eq!(loaded.node_count, 2);
        assert_eq!(loaded.nodes.len(), 2);
    }
}
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod graph;
```

**Step 3: Run tests**

Run: `cargo test --lib graph`
Expected: Tests pass

**Step 4: Commit**

```bash
git add src/storage/index_persistence/graph.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement graph index persistence with property conversion"
```

---

## Task 7: Implement Temporal Index Persistence

**Files:**
- Create: `src/storage/index_persistence/temporal.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Create temporal persistence**

Create `src/storage/index_persistence/temporal.rs`:

```rust
//! Temporal index persistence.

use std::fs;
use std::path::Path;

use super::error::{IndexPersistenceError, Result};
use super::formats::TemporalIndexData;
use super::{MANIFEST_VERSION, TEMPORAL_MAGIC};

/// Save temporal index data to disk.
pub fn save_temporal_index(data: &TemporalIndexData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(data)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load temporal index data from disk.
pub fn load_temporal_index(path: &Path) -> Result<TemporalIndexData> {
    let bytes = fs::read(path)?;
    let data: TemporalIndexData = bitcode::decode(&bytes)?;

    if data.magic != TEMPORAL_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: TEMPORAL_MAGIC,
            got: data.magic,
        });
    }

    if data.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: data.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(data)
}

/// Create a new empty TemporalIndexData.
pub fn new_temporal_index_data() -> TemporalIndexData {
    TemporalIndexData {
        magic: TEMPORAL_MAGIC,
        version: MANIFEST_VERSION,
        node_versions: Vec::new(),
        node_anchors: Vec::new(),
        edge_versions: Vec::new(),
        edge_anchors: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::*;
    use tempfile::tempdir;

    #[test]
    fn test_temporal_index_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("temporal.idx");

        let mut data = new_temporal_index_data();
        data.node_versions.push(NodeVersionEntry {
            node_id: 1,
            valid_from: 1000,
            valid_to: Some(2000),
            tx_time: 1000,
            version_type: PersistedVersionType::Anchor,
            properties: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
        });
        data.node_anchors.push(NodeAnchorEntry {
            node_id: 1,
            anchor_tx_time: 1000,
            full_state: PersistedPropertyMap { entries: vec![] },
            vector_snapshot_id: Some(42),
        });

        save_temporal_index(&data, &path).unwrap();
        let loaded = load_temporal_index(&path).unwrap();

        assert_eq!(loaded.node_versions.len(), 1);
        assert_eq!(loaded.node_anchors.len(), 1);
        assert_eq!(loaded.node_versions[0].vector_snapshot_id, Some(42));
    }
}
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod temporal;
```

**Step 3: Run tests**

Run: `cargo test --lib temporal_index_round_trip`
Expected: Test passes

**Step 4: Commit**

```bash
git add src/storage/index_persistence/temporal.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement temporal index persistence"
```

---

## Task 8: Implement Vector Index Persistence

**Files:**
- Create: `src/storage/index_persistence/vector.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Create vector persistence (metadata + mappings)**

Create `src/storage/index_persistence/vector.rs`:

```rust
//! Vector index persistence.
//!
//! Vector indexes use a hybrid approach:
//! - usearch native format for the HNSW index itself
//! - bitcode for metadata and ID mappings

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::error::{IndexPersistenceError, Result};
use super::formats::{
    PersistedHnswConfig, VectorIndexMeta, VectorMapping, VectorMappingsData, VectorSnapshotMeta,
};
use super::{MANIFEST_VERSION, VECTOR_META_MAGIC};

/// Save vector index metadata.
pub fn save_vector_meta(meta: &VectorIndexMeta, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(meta)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load vector index metadata.
pub fn load_vector_meta(path: &Path) -> Result<VectorIndexMeta> {
    let bytes = fs::read(path)?;
    let meta: VectorIndexMeta = bitcode::decode(&bytes)?;

    if meta.magic != VECTOR_META_MAGIC {
        return Err(IndexPersistenceError::InvalidMagic {
            path: path.to_path_buf(),
            expected: VECTOR_META_MAGIC,
            got: meta.magic,
        });
    }

    if meta.version > MANIFEST_VERSION {
        return Err(IndexPersistenceError::UnsupportedVersion {
            found: meta.version,
            supported: MANIFEST_VERSION,
        });
    }

    Ok(meta)
}

/// Save vector ID mappings.
pub fn save_vector_mappings(mappings: &VectorMappingsData, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(mappings)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load vector ID mappings.
pub fn load_vector_mappings(path: &Path) -> Result<VectorMappingsData> {
    let bytes = fs::read(path)?;
    let mappings: VectorMappingsData = bitcode::decode(&bytes)?;
    Ok(mappings)
}

/// Save vector snapshot metadata.
pub fn save_snapshot_meta(meta: &VectorSnapshotMeta, path: &Path) -> Result<()> {
    let encoded = bitcode::encode(meta)?;
    fs::write(path, encoded)?;
    Ok(())
}

/// Load vector snapshot metadata.
pub fn load_snapshot_meta(path: &Path) -> Result<VectorSnapshotMeta> {
    let bytes = fs::read(path)?;
    let meta: VectorSnapshotMeta = bitcode::decode(&bytes)?;
    Ok(meta)
}

/// Create new vector index metadata.
pub fn new_vector_meta(
    property_name: &str,
    dimensions: u32,
    metric: u8,
    config: PersistedHnswConfig,
) -> VectorIndexMeta {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    VectorIndexMeta {
        magic: VECTOR_META_MAGIC,
        version: MANIFEST_VERSION,
        property_name: property_name.to_string(),
        dimensions,
        metric,
        hnsw_config: config,
        vector_count: 0,
        created_at: now,
        last_modified: now,
    }
}

/// Create empty vector mappings.
pub fn new_vector_mappings() -> VectorMappingsData {
    VectorMappingsData {
        version: MANIFEST_VERSION,
        count: 0,
        mappings: Vec::new(),
        deleted_ids: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index_persistence::formats::PersistedSnapshotType;
    use tempfile::tempdir;

    #[test]
    fn test_vector_meta_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("meta.idx");

        let config = PersistedHnswConfig {
            m: 16,
            ef_construction: 128,
            ef_search: 64,
        };
        let meta = new_vector_meta("embedding", 384, 0, config);

        save_vector_meta(&meta, &path).unwrap();
        let loaded = load_vector_meta(&path).unwrap();

        assert_eq!(loaded.property_name, "embedding");
        assert_eq!(loaded.dimensions, 384);
        assert_eq!(loaded.hnsw_config.m, 16);
    }

    #[test]
    fn test_vector_mappings_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mappings.idx");

        let mut mappings = new_vector_mappings();
        mappings.count = 3;
        mappings.mappings.push(VectorMapping {
            node_id: 1,
            usearch_key: 100,
        });
        mappings.mappings.push(VectorMapping {
            node_id: 2,
            usearch_key: 101,
        });
        mappings.mappings.push(VectorMapping {
            node_id: 3,
            usearch_key: 102,
        });
        mappings.deleted_ids.push(99);

        save_vector_mappings(&mappings, &path).unwrap();
        let loaded = load_vector_mappings(&path).unwrap();

        assert_eq!(loaded.count, 3);
        assert_eq!(loaded.mappings.len(), 3);
        assert_eq!(loaded.deleted_ids, vec![99]);
    }

    #[test]
    fn test_snapshot_meta_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("snapshot.meta");

        let meta = VectorSnapshotMeta {
            snapshot_id: 42,
            snapshot_type: PersistedSnapshotType::Full,
            timestamp: 1234567890,
            vector_count: 1000,
            config: PersistedHnswConfig {
                m: 16,
                ef_construction: 128,
                ef_search: 64,
            },
            base_snapshot_id: None,
        };

        save_snapshot_meta(&meta, &path).unwrap();
        let loaded = load_snapshot_meta(&path).unwrap();

        assert_eq!(loaded.snapshot_id, 42);
        assert_eq!(loaded.vector_count, 1000);
        assert!(matches!(loaded.snapshot_type, PersistedSnapshotType::Full));
    }
}
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod vector;
```

**Step 3: Run tests**

Run: `cargo test --lib vector`
Expected: Tests pass

**Step 4: Commit**

```bash
git add src/storage/index_persistence/vector.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement vector index metadata and mappings persistence"
```

---

## Task 9: Implement Index Loader

**Files:**
- Create: `src/storage/index_persistence/loader.rs`
- Modify: `src/storage/index_persistence/mod.rs`

**Step 1: Create the index loader**

Create `src/storage/index_persistence/loader.rs`:

```rust
//! Index loading and directory management.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::{IndexPersistenceError, Result};
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
    /// 1. String interner (others depend on it)
    /// 2. Manifest
    /// 3. Other indexes can be loaded in parallel after this
    pub fn load_manifest_and_strings(&self) -> Result<IndexManifest> {
        // 1. Load and restore string interner first
        let interner_path = self.interner_path();
        if interner_path.exists() {
            let interner_data = load_string_interner(&interner_path)?;
            restore_string_interner(&interner_data)?;
        }

        // 2. Load manifest
        let manifest = load_manifest(&self.manifest_path())?;

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
        GLOBAL_INTERNER.intern("test_label");

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
```

**Step 2: Export from module**

Add to `src/storage/index_persistence/mod.rs`:

```rust
pub mod loader;

pub use loader::IndexPersistenceManager;
```

**Step 3: Run tests**

Run: `cargo test --lib loader`
Expected: Tests pass

**Step 4: Commit**

```bash
git add src/storage/index_persistence/loader.rs src/storage/index_persistence/mod.rs
git commit -m "feat: implement index persistence manager and loader"
```

---

## Task 10: Add Public API and Configuration

**Files:**
- Create: `src/storage/index_persistence/api.rs`
- Modify: `src/storage/index_persistence/mod.rs`
- Modify: `src/config.rs`

**Step 1: Create public API types**

Create `src/storage/index_persistence/api.rs`:

```rust
//! Public API types for index persistence.

use std::path::PathBuf;
use std::time::Duration;

use super::formats::PersistencePolicies;

/// Configuration for index persistence.
#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// Whether persistence is enabled
    pub enabled: bool,
    /// Base data directory
    pub data_dir: PathBuf,
    /// Persistence trigger policies
    pub policies: PersistencePolicies,
    /// Whether to load indexes on startup
    pub load_on_startup: bool,
    /// Whether to use memory-mapped loading
    pub use_mmap: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            data_dir: PathBuf::from("data"),
            policies: PersistencePolicies::default(),
            load_on_startup: true,
            use_mmap: true,
        }
    }
}

/// Statistics from a persistence operation.
#[derive(Debug, Clone)]
pub struct PersistenceStats {
    /// Time taken for persistence
    pub duration: Duration,
    /// Total bytes written
    pub bytes_written: u64,
    /// Names of indexes that were persisted
    pub indexes_persisted: Vec<String>,
}

/// Status of the persistence layer.
#[derive(Debug, Clone)]
pub struct PersistenceStatus {
    /// LSN in the manifest
    pub manifest_lsn: u64,
    /// Current database LSN
    pub current_lsn: u64,
    /// Status of each vector index
    pub vector_indexes: Vec<VectorIndexStatus>,
    /// Status of graph index
    pub graph_index: Option<IndexStatus>,
    /// Status of temporal index
    pub temporal_index: Option<IndexStatus>,
    /// Status of string interner
    pub string_interner: Option<IndexStatus>,
}

/// Status of an individual index.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    /// When the index was last persisted
    pub last_persisted: Option<i64>,
    /// Number of mutations since last persist
    pub mutations_since_persist: u64,
    /// Whether the index has unpersisted changes
    pub dirty: bool,
    /// Size of the index on disk
    pub size_bytes: u64,
}

/// Status of a vector index.
#[derive(Debug, Clone)]
pub struct VectorIndexStatus {
    /// Property name
    pub property_name: String,
    /// Index status
    pub status: IndexStatus,
    /// Number of temporal snapshots
    pub snapshot_count: u32,
}
```

**Step 2: Export from module**

Update `src/storage/index_persistence/mod.rs` to add:

```rust
pub mod api;

pub use api::{PersistenceConfig, PersistenceStats, PersistenceStatus, IndexStatus, VectorIndexStatus};
```

**Step 3: Add to GallifreyDBConfig**

In `src/config.rs`, add after the existing config fields (find the `GallifreyDBConfig` struct):

```rust
use crate::storage::index_persistence::PersistenceConfig;

// In GallifreyDBConfig struct, add:
    /// Index persistence configuration
    pub persistence: PersistenceConfig,

// In Default impl, add:
    persistence: PersistenceConfig::default(),

// In builder, add:
    /// Set persistence configuration
    pub fn persistence(mut self, config: PersistenceConfig) -> Self {
        self.persistence = config;
        self
    }
```

**Step 4: Run tests**

Run: `cargo test --lib`
Expected: All tests pass

**Step 5: Commit**

```bash
git add src/storage/index_persistence/api.rs src/storage/index_persistence/mod.rs src/config.rs
git commit -m "feat: add persistence config and public API types"
```

---

## Task 11: Integration Test - Full Persistence Cycle

**Files:**
- Create: `tests/index_persistence.rs`

**Step 1: Create integration test**

Create `tests/index_persistence.rs`:

```rust
//! Integration tests for index persistence.

use gallifreydb::storage::index_persistence::{
    formats::*, graph::*, loader::IndexPersistenceManager, manifest::*, strings::*, temporal::*,
    vector::*, IndexPersistenceError, GRAPH_MAGIC, INTERNER_MAGIC, MANIFEST_MAGIC, TEMPORAL_MAGIC,
    VECTOR_META_MAGIC,
};
use gallifreydb::core::GLOBAL_INTERNER;
use tempfile::tempdir;

#[test]
fn test_full_persistence_cycle() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());

    // Step 1: Intern some strings (simulating normal DB operation)
    let label1_idx = GLOBAL_INTERNER.intern("Person");
    let label2_idx = GLOBAL_INTERNER.intern("Document");
    let prop_idx = GLOBAL_INTERNER.intern("name");

    // Step 2: Create and save string interner
    manager.save_string_interner().unwrap();

    // Step 3: Create graph index data
    let mut graph_data = new_graph_index_data();
    graph_data.node_count = 2;
    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: label1_idx,
        properties: PersistedPropertyMap {
            entries: vec![(prop_idx, PersistedPropertyValue::String(prop_idx))],
        },
    });
    graph_data.nodes.push(PersistedNode {
        id: 2,
        label_idx: label2_idx,
        properties: PersistedPropertyMap { entries: vec![] },
    });
    save_graph_index(&graph_data, &manager.graph_path().join("adjacency.idx")).unwrap();

    // Step 4: Create temporal index data
    let mut temporal_data = new_temporal_index_data();
    temporal_data.node_versions.push(NodeVersionEntry {
        node_id: 1,
        valid_from: 1000,
        valid_to: None,
        tx_time: 1000,
        version_type: PersistedVersionType::Anchor,
        properties: PersistedPropertyMap { entries: vec![] },
        vector_snapshot_id: None,
    });
    save_temporal_index(&temporal_data, &manager.temporal_path().join("versions.idx")).unwrap();

    // Step 5: Create vector index metadata
    let vector_dir = manager.vector_path("embedding");
    std::fs::create_dir_all(&vector_dir).unwrap();

    let config = PersistedHnswConfig {
        m: 16,
        ef_construction: 128,
        ef_search: 64,
    };
    let meta = new_vector_meta("embedding", 384, 0, config);
    save_vector_meta(&meta, &vector_dir.join("current.meta")).unwrap();

    let mut mappings = new_vector_mappings();
    mappings.count = 1;
    mappings.mappings.push(VectorMapping {
        node_id: 1,
        usearch_key: 0,
    });
    save_vector_mappings(&mappings, &vector_dir.join("current.mappings")).unwrap();

    // Step 6: Create and save manifest
    let mut manifest = IndexManifest::new(42);
    manifest.string_interner = Some(StringInternerManifestEntry {
        interner_file: "strings/interner.idx".to_string(),
        string_count: 3,
    });
    manifest.graph_index = Some(GraphIndexManifestEntry {
        adjacency_file: "graph/adjacency.idx".to_string(),
        node_count: 2,
        edge_count: 0,
    });
    manifest.temporal_index = Some(TemporalIndexManifestEntry {
        node_versions_file: "temporal/versions.idx".to_string(),
        edge_versions_file: "temporal/edge_versions.idx".to_string(),
        version_count: 1,
    });
    manifest.vector_indexes.push(VectorIndexManifestEntry {
        property_name: "embedding".to_string(),
        dimensions: 384,
        metric: 0,
        current_file: "vector/embedding/current.usearch".to_string(),
        mappings_file: "vector/embedding/current.mappings".to_string(),
        snapshot_count: 0,
        temporal_enabled: false,
    });
    manager.save_manifest(&manifest).unwrap();

    // Step 7: Verify everything was saved
    assert!(manager.manifest_path().exists());
    assert!(manager.interner_path().exists());
    assert!(manager.graph_path().join("adjacency.idx").exists());
    assert!(manager.temporal_path().join("versions.idx").exists());
    assert!(vector_dir.join("current.meta").exists());
    assert!(vector_dir.join("current.mappings").exists());

    // Step 8: Load everything back
    let loaded_manifest = manager.load_manifest_and_strings().unwrap();

    assert_eq!(loaded_manifest.lsn, 42);
    assert_eq!(loaded_manifest.vector_indexes.len(), 1);
    assert_eq!(loaded_manifest.vector_indexes[0].property_name, "embedding");
    assert!(loaded_manifest.graph_index.is_some());
    assert!(loaded_manifest.temporal_index.is_some());
    assert!(loaded_manifest.string_interner.is_some());

    // Step 9: Verify string interner was restored correctly
    assert_eq!(GLOBAL_INTERNER.resolve(label1_idx), Some("Person".to_string()));
    assert_eq!(GLOBAL_INTERNER.resolve(label2_idx), Some("Document".to_string()));
}

#[test]
fn test_missing_manifest_returns_error() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());

    let result = manager.load_manifest_and_strings();
    assert!(result.is_err());
}

#[test]
fn test_corrupted_file_returns_error() {
    let dir = tempdir().unwrap();
    let manager = IndexPersistenceManager::new(dir.path());
    manager.ensure_directories().unwrap();

    // Write garbage to manifest
    std::fs::write(manager.manifest_path(), b"not valid bitcode").unwrap();

    let result = manager.load_manifest_and_strings();
    assert!(result.is_err());
}
```

**Step 2: Run integration tests**

Run: `cargo test --test index_persistence`
Expected: All tests pass

**Step 3: Commit**

```bash
git add tests/index_persistence.rs
git commit -m "test: add integration tests for index persistence cycle"
```

---

## Task 12: Final Cleanup and Documentation

**Files:**
- Modify: `src/storage/index_persistence/mod.rs`
- Modify: `src/lib.rs`

**Step 1: Add module documentation**

Update the module doc comment in `src/storage/index_persistence/mod.rs`:

```rust
//! Comprehensive index persistence layer for GallifreyDB.
//!
//! This module provides persistence for all index types using bitcode serialization:
//! - **Vector indexes**: HNSW via usearch native format + bitcode metadata
//! - **Graph indexes**: CSR adjacency with bitcode
//! - **Temporal indexes**: Version chains with bitcode
//! - **String interner**: Deduplication table with bitcode
//!
//! # Architecture
//!
//! ```text
//! data/
//! └── indexes/
//!     ├── manifest.idx          # Index registry (bitcode)
//!     ├── strings/interner.idx  # String interning table (bitcode)
//!     ├── graph/adjacency.idx   # CSR adjacency data (bitcode)
//!     ├── temporal/*.idx        # Version chains (bitcode)
//!     └── vector/{prop}/        # Per-property vector indexes
//!         ├── current.usearch   # HNSW index (usearch native)
//!         ├── current.meta      # Index metadata (bitcode)
//!         ├── current.mappings  # NodeId <-> key mappings (bitcode)
//!         └── snapshots/        # Temporal snapshots
//! ```
//!
//! # Load Order
//!
//! Indexes must be loaded in this order due to dependencies:
//!
//! 1. **String interner** - Other indexes reference string indices
//! 2. **Manifest** - Tells us what indexes exist and their locations
//! 3. **Graph, Temporal, Vector** - Can be loaded in parallel
//!
//! # Example
//!
//! ```no_run
//! use gallifreydb::storage::index_persistence::IndexPersistenceManager;
//!
//! let manager = IndexPersistenceManager::new("data");
//!
//! // Save all indexes
//! manager.save_string_interner().unwrap();
//! // ... save other indexes ...
//! manager.save_manifest(&manifest).unwrap();
//!
//! // Load indexes on startup
//! if manager.indexes_exist() {
//!     let manifest = manager.load_manifest_and_strings().unwrap();
//!     // ... load other indexes based on manifest ...
//! }
//! ```
//!
//! # Design Documents
//!
//! - [Design](../../../docs/plans/2026-01-15-index-persistence-design.md)
//! - [ADR-0023](../../../docs/adr/0023-index-persistence-layer.md)
```

**Step 2: Export from lib.rs**

Verify `src/storage/mod.rs` exports `index_persistence`:

```rust
pub mod index_persistence;
```

**Step 3: Run all tests**

Run: `cargo test --lib && cargo test --tests`
Expected: All tests pass

**Step 4: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: No warnings

**Step 5: Format code**

Run: `cargo fmt --all`

**Step 6: Final commit**

```bash
git add -A
git commit -m "docs: add comprehensive module documentation for index persistence"
```

---

## Summary

This implementation plan covers:

1. **Dependencies**: bitcode, memmap2
2. **Module structure**: `src/storage/index_persistence/` with 8 submodules
3. **Format structs**: All bitcode-serializable types for each index
4. **Persistence functions**: Save/load for each index type
5. **Index loader**: Directory management and load orchestration
6. **Public API**: Config, stats, and status types
7. **Integration tests**: Full persistence cycle verification

**Total commits**: 12
**Estimated lines of code**: ~1500

After completing this plan, the next steps would be:
1. Integrate with `GallifreyDB` struct for automatic persistence
2. Add trigger hooks for index-specific persistence policies
3. Implement memory-mapped loading with copy-on-write
4. Add recovery logic (rebuild from WAL on corruption)
