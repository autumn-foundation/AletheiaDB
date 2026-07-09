//! Bitcode-serializable format structs for index persistence.
//!
//! This module defines the schema for all persisted index files. These structs are
//! serialized using [bitcode](https://github.com/llogiq/bitcode), a fast and compact
//! binary serialization format.
//!
//! # Schema Overview
//!
//! | Struct | Corresponding File | Description |
//! |--------|-------------------|-------------|
//! | [`IndexManifest`] | `manifest.idx` | Root registry of all indexes |
//! | [`StringInternerData`] | `strings/interner.idx` | Interned strings table |
//! | [`GraphIndexData`] | `graph/adjacency.idx` | Graph structure and properties |
//! | [`GraphIndexDelta`] | `graph/delta.idx` | Incremental graph changes |
//! | [`TemporalIndexData`] | `temporal/versions.idx` | Historical version chains |
//! | [`VectorIndexMeta`] | `vector/{prop}/meta.idx` | Vector index metadata |
//! | [`VectorMappingsData`] | `vector/{prop}/mappings.idx` | Vector ID mappings |

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
    /// WAL position this manifest is consistent with.
    ///
    /// # Semantics (Issue #3419)
    ///
    /// This is the **next-to-allocate** LSN (`wal.current_lsn()`) captured
    /// *before* the snapshot was taken — NOT the last-allocated LSN. The
    /// resulting contract is:
    ///
    /// - Every WAL entry with LSN **< lsn** is guaranteed to be reflected in
    ///   the persisted snapshot.
    /// - Entries with LSN **>= lsn** may or may not be reflected (a write can
    ///   race a background persist between the LSN capture and the snapshot
    ///   read).
    ///
    /// Startup replay must therefore begin **AT `lsn` (inclusive)** — never
    /// at `lsn + 1`, which silently drops the first post-persist write — and
    /// WAL replay must be idempotent for entries whose effects are already in
    /// the snapshot (see the re-application guards in
    /// `storage::recovery::replay_wal_into_storage_with_constraints`).
    ///
    /// The allocator must also never allocate below this value after a
    /// restart (Issue #3420); startup seeds it from
    /// `max(max LSN in WAL segments + 1, manifest lsn)`.
    pub lsn: u64,

    /// Vector index entries (one per property)
    pub vector_indexes: Vec<VectorIndexManifestEntry>,
    /// Graph index entry
    pub graph_index: Option<GraphIndexManifestEntry>,
    /// Temporal index entry
    pub temporal_index: Option<TemporalIndexManifestEntry>,
    /// Temporal adjacency index entry
    pub temporal_adjacency_index: Option<TemporalAdjacencyIndexManifestEntry>,
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

/// Manifest entry for temporal adjacency index.
#[derive(Debug, Clone, Encode, Decode)]
pub struct TemporalAdjacencyIndexManifestEntry {
    /// Relative path to temporal adjacency file
    pub adjacency_file: String,
    /// Total number of entries
    pub entry_count: u64,
    /// Number of nodes with outgoing edges
    pub node_count: u64,
}

// ============================================================================
// String Interner Format
// ============================================================================

/// Persisted string interner data.
///
/// FORMAT-FROZEN: reused verbatim inside `BackupPayloadV1`/`BackupPayloadV2`
/// (`storage::backup`). Changing this struct's wire layout requires freezing
/// a copy for those legacy artifact shapes first.
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
///
/// FORMAT-FROZEN: reused verbatim inside `BackupPayloadV1`/`BackupPayloadV2`
/// (`storage::backup`). Changing this struct's wire layout requires freezing
/// a copy for those legacy artifact shapes first.
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

    /// CSR outgoing adjacency: sorted node IDs with outgoing edges
    pub outgoing_node_ids: Vec<u64>,
    /// CSR outgoing adjacency: offsets into neighbors array
    pub outgoing_offsets: Vec<u64>,
    /// CSR outgoing adjacency: packed edge IDs
    pub outgoing_neighbors: Vec<u64>,

    /// CSR incoming adjacency: sorted node IDs with incoming edges
    pub incoming_node_ids: Vec<u64>,
    /// CSR incoming adjacency: offsets into neighbors array
    pub incoming_offsets: Vec<u64>,
    /// CSR incoming adjacency: packed edge IDs
    pub incoming_neighbors: Vec<u64>,
}

/// Delta encoding for incremental graph index saves.
///
/// Stores only the changes between a base snapshot and a modified version,
/// enabling smaller incremental saves. Tracks additions, modifications, and deletions.
#[derive(Debug, Clone, Encode, Decode)]
pub struct GraphIndexDelta {
    /// Magic bytes: "GDLT"
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,

    /// Nodes added since base
    pub added_nodes: Vec<PersistedNode>,
    /// Nodes modified since base (full new state)
    pub modified_nodes: Vec<PersistedNode>,
    /// Node IDs deleted since base
    pub deleted_node_ids: Vec<u64>,

    /// Edges added since base
    pub added_edges: Vec<PersistedEdge>,
    /// Edges modified since base (full new state)
    pub modified_edges: Vec<PersistedEdge>,
    /// Edge IDs deleted since base
    pub deleted_edge_ids: Vec<u64>,

    /// New node count after applying delta
    pub new_node_count: u64,
    /// New edge count after applying delta
    pub new_edge_count: u64,
}

/// Persisted node data.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct PersistedNode {
    /// Node ID
    pub id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Current version ID (links to historical storage)
    /// CRITICAL: This must be preserved to maintain temporal provenance
    pub version_id: u64,
    /// Node properties
    pub properties: PersistedPropertyMap,
}

/// Persisted edge data.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
pub struct PersistedEdge {
    /// Edge ID
    pub id: u64,
    /// Source node ID
    pub source_id: u64,
    /// Target node ID
    pub target_id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Current version ID (links to historical storage)
    /// CRITICAL: This must be preserved to maintain temporal provenance
    pub version_id: u64,
    /// Edge properties
    pub properties: PersistedPropertyMap,
}

/// Persisted property map.
///
/// FORMAT-FROZEN: reused verbatim by the frozen legacy temporal shapes
/// (`legacy_v1`, `legacy_v2`, `legacy_v3`) and the legacy backup payloads
/// (`BackupPayloadV1`/`V2`). Changing its wire layout silently changes what
/// those frozen shapes decode -- freeze a copy for them first.
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
pub struct PersistedPropertyMap {
    /// Property entries: (key_index, value)
    pub entries: Vec<(u32, PersistedPropertyValue)>,
}

/// Persisted property value.
///
/// Note: Array and Map variants are currently not supported due to
/// bitcode recursion limitations. These will be added in a future update.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
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
}

// ============================================================================
// Temporal Index Format
// ============================================================================

/// Persisted write-time attributive provenance bundle (Issue #3224).
///
/// Mirrors [`crate::core::provenance::Provenance`]'s fields exactly; kept as
/// a separate bitcode-encodable type since `Provenance` itself has private
/// fields and validates on construction (not on persistence).
///
/// FORMAT-FROZEN: reused verbatim by `legacy_v3` and `BackupPayloadV3`
/// (the pre-#3350 shape is separately frozen as
/// `legacy_v2::PersistedProvenanceV2`). Changing its wire layout requires
/// freezing a copy for those shapes first.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistedProvenance {
    /// Source system/identifier that produced the write, if any.
    pub source: Option<String>,
    /// Confidence in `[0.0, 1.0]`, if any.
    pub confidence: Option<f64>,
    /// Free-text note, if any.
    pub note: Option<String>,
    /// Correlation ID grouping co-committed writes, if any.
    pub correlation_id: Option<String>,
    /// Authenticated principal that made the write (Issue #3350), if any.
    pub principal: Option<String>,
}

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
    /// Unique version identifier (preserved from original)
    pub version_id: u64,
    /// Node ID
    pub node_id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Valid time start (unix timestamp)
    pub valid_from: i64,
    /// Valid time end (None = still valid)
    pub valid_to: Option<i64>,
    /// Valid time start (logical counter)
    pub valid_from_logical: u32,
    /// Valid time end (logical counter)
    pub valid_to_logical: Option<u32>,
    /// Transaction time (unix timestamp)
    pub tx_time: i64,
    /// Transaction time (logical counter)
    pub tx_time_logical: u32,
    /// Version type (delta or anchor)
    pub version_type: PersistedVersionType,
    /// Properties at this version
    pub properties: PersistedPropertyMap,
    /// Vector snapshot ID for provenance tracking
    pub vector_snapshot_id: Option<u64>,
    /// Write-time attributive provenance bundle (Issue #3224), if supplied.
    pub provenance: Option<PersistedProvenance>,
    /// Transaction time end (None = still current knowledge) (Issue #3387).
    pub tx_end: Option<i64>,
    /// Transaction time end (logical counter) (Issue #3387).
    pub tx_end_logical: Option<u32>,
    /// Previous version in this node's chain (earlier tx time) (Issue #3387).
    pub prev_version: Option<u64>,
    /// Next version in this node's chain (later tx time) (Issue #3387).
    pub next_version: Option<u64>,
}

/// Persisted node anchor entry.
///
/// FORMAT-FROZEN: reused verbatim (unchanged since v1) by the frozen legacy
/// temporal shapes (`legacy_v1`, `legacy_v2`, `legacy_v3`) and the legacy backup
/// payloads. Changing its wire layout requires freezing a copy first.
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
///
/// FORMAT-FROZEN: reused verbatim by the frozen legacy temporal shapes
/// (`legacy_v1`, `legacy_v2`, `legacy_v3`) and the legacy backup payloads. Changing its
/// wire layout requires freezing a copy for those shapes first.
#[derive(Debug, Clone, Encode, Decode)]
pub enum PersistedVersionType {
    /// Delta referencing a base anchor
    Delta {
        /// Transaction time of base anchor
        base_anchor_tx: i64,
        /// Transaction time of base anchor (logical counter)
        base_anchor_tx_logical: u32,
        /// Property keys that were removed in this delta (interned string indices)
        removed_keys: Vec<u32>,
    },
    /// Full anchor snapshot
    Anchor,
}

/// Persisted edge version entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct EdgeVersionEntry {
    /// Unique version identifier (preserved from original)
    pub version_id: u64,
    /// Edge ID
    pub edge_id: u64,
    /// Source node ID
    pub source_id: u64,
    /// Target node ID
    pub target_id: u64,
    /// Label index in string interner
    pub label_idx: u32,
    /// Valid time start
    pub valid_from: i64,
    /// Valid time end
    pub valid_to: Option<i64>,
    /// Valid time start (logical counter)
    pub valid_from_logical: u32,
    /// Valid time end (logical counter)
    pub valid_to_logical: Option<u32>,
    /// Transaction time
    pub tx_time: i64,
    /// Transaction time (logical counter)
    pub tx_time_logical: u32,
    /// Version type
    pub version_type: PersistedVersionType,
    /// Properties
    pub properties: PersistedPropertyMap,
    /// Write-time attributive provenance bundle (Issue #3224), if supplied.
    pub provenance: Option<PersistedProvenance>,
    /// Transaction time end (None = still current knowledge) (Issue #3387).
    pub tx_end: Option<i64>,
    /// Transaction time end (logical counter) (Issue #3387).
    pub tx_end_logical: Option<u32>,
    /// Previous version in this edge's chain (earlier tx time) (Issue #3387).
    pub prev_version: Option<u64>,
    /// Next version in this edge's chain (later tx time) (Issue #3387).
    pub next_version: Option<u64>,
}

/// Persisted edge anchor entry.
///
/// FORMAT-FROZEN: reused verbatim (unchanged since v1) by the frozen legacy
/// temporal shapes (`legacy_v1`, `legacy_v2`, `legacy_v3`) and the legacy backup
/// payloads. Changing its wire layout requires freezing a copy first.
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
// Temporal Adjacency Index Format
// ============================================================================

/// Temporal adjacency index data - maps (node_id, time) -> edge_ids.
///
/// Note: Only outgoing edges are persisted. The incoming index is automatically
/// rebuilt during load via insert_edge(), which populates both directions.
#[derive(Debug, Clone, Encode, Decode)]
pub struct TemporalAdjacencyData {
    /// Magic bytes: "GTAJ" (Graph Temporal Adjacency)
    pub magic: [u8; 4],
    /// Format version
    pub version: u16,

    /// Outgoing edges per node (incoming is rebuilt during load)
    pub outgoing: Vec<NodeAdjacencyEntry>,
}

/// Adjacency entries for a single node.
#[derive(Debug, Clone, Encode, Decode)]
pub struct NodeAdjacencyEntry {
    /// Node ID
    pub node_id: u64,
    /// Temporal adjacency entries for this node
    pub entries: Vec<PersistedTemporalAdjacencyEntry>,
}

/// Persisted temporal adjacency entry.
#[derive(Debug, Clone, Encode, Decode)]
pub struct PersistedTemporalAdjacencyEntry {
    /// Edge ID
    pub edge_id: u64,
    /// Neighbor node (target for outgoing, source for incoming)
    pub neighbor: u64,
    /// Edge label (interned string ID)
    pub label: u32,
    /// Valid time range start - wallclock component (microseconds since Unix epoch)
    pub valid_from_wallclock: i64,
    /// Valid time range start - logical counter
    pub valid_from_logical: u32,
    /// Valid time range end - wallclock component
    pub valid_to_wallclock: i64,
    /// Valid time range end - logical counter
    pub valid_to_logical: u32,
    /// Transaction time range start - wallclock component
    pub tx_from_wallclock: i64,
    /// Transaction time range start - logical counter
    pub tx_from_logical: u32,
    /// Transaction time range end - wallclock component
    pub tx_to_wallclock: i64,
    /// Transaction time range end - logical counter
    pub tx_to_logical: u32,
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

/// Fully loaded vector index data.
#[derive(Debug, Clone)]
pub struct VectorIndexData {
    /// Metadata
    pub meta: VectorIndexMeta,
    /// ID Mappings
    pub mappings: VectorMappingsData,
    /// Path to the usearch index file
    pub index_path: std::path::PathBuf,
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
    /// AletheiaDB node ID
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
#[derive(Debug, Clone, PartialEq, Default, Encode, Decode)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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

/// Vector index persistence policy.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
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

// ============================================================================
// Legacy (pre-provenance) Temporal Index Format (Issue #3224)
// ============================================================================

/// Frozen copies of the temporal index structs as they existed before
/// write-time provenance (Issue #3224) was added, i.e. `MANIFEST_VERSION == 1`.
///
/// `bitcode` is a positional, non-self-describing format: appending a field
/// to [`NodeVersionEntry`]/[`EdgeVersionEntry`] changes their wire layout, so
/// files written by older binaries can no longer decode as the current
/// structs. These frozen shapes let [`super::temporal::load_temporal_index`]
/// fall back to the old layout and upgrade in memory (`provenance: None`).
///
/// Do not modify these types after they're introduced -- they exist purely
/// to describe historical on-disk bytes.
pub mod legacy_v1 {
    use super::{EdgeAnchorEntry, NodeAnchorEntry, PersistedPropertyMap, PersistedVersionType};
    use bitcode::{Decode, Encode};

    /// Pre-provenance `TemporalIndexData` (`version == 1`).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct TemporalIndexDataV1 {
        /// Magic bytes: "GTMP"
        pub magic: [u8; 4],
        /// Format version (always 1 for this shape)
        pub version: u16,
        /// Node version entries
        pub node_versions: Vec<NodeVersionEntryV1>,
        /// Node anchor entries (unchanged since v1)
        pub node_anchors: Vec<NodeAnchorEntry>,
        /// Edge version entries
        pub edge_versions: Vec<EdgeVersionEntryV1>,
        /// Edge anchor entries (unchanged since v1)
        pub edge_anchors: Vec<EdgeAnchorEntry>,
    }

    /// Pre-provenance `NodeVersionEntry` (no `provenance` field).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct NodeVersionEntryV1 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Node ID
        pub node_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start (unix timestamp)
        pub valid_from: i64,
        /// Valid time end (None = still valid)
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time (unix timestamp)
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type (delta or anchor)
        pub version_type: PersistedVersionType,
        /// Properties at this version
        pub properties: PersistedPropertyMap,
        /// Vector snapshot ID for provenance tracking
        pub vector_snapshot_id: Option<u64>,
    }

    /// Pre-provenance `EdgeVersionEntry` (no `provenance` field).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct EdgeVersionEntryV1 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Edge ID
        pub edge_id: u64,
        /// Source node ID
        pub source_id: u64,
        /// Target node ID
        pub target_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start
        pub valid_from: i64,
        /// Valid time end
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type
        pub version_type: PersistedVersionType,
        /// Properties
        pub properties: PersistedPropertyMap,
    }

    impl From<NodeVersionEntryV1> for super::NodeVersionEntry {
        fn from(v1: NodeVersionEntryV1) -> Self {
            super::NodeVersionEntry {
                version_id: v1.version_id,
                node_id: v1.node_id,
                label_idx: v1.label_idx,
                valid_from: v1.valid_from,
                valid_to: v1.valid_to,
                valid_from_logical: v1.valid_from_logical,
                valid_to_logical: v1.valid_to_logical,
                tx_time: v1.tx_time,
                tx_time_logical: v1.tx_time_logical,
                version_type: v1.version_type,
                properties: v1.properties,
                vector_snapshot_id: v1.vector_snapshot_id,
                provenance: None,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<EdgeVersionEntryV1> for super::EdgeVersionEntry {
        fn from(v1: EdgeVersionEntryV1) -> Self {
            super::EdgeVersionEntry {
                version_id: v1.version_id,
                edge_id: v1.edge_id,
                source_id: v1.source_id,
                target_id: v1.target_id,
                label_idx: v1.label_idx,
                valid_from: v1.valid_from,
                valid_to: v1.valid_to,
                valid_from_logical: v1.valid_from_logical,
                valid_to_logical: v1.valid_to_logical,
                tx_time: v1.tx_time,
                tx_time_logical: v1.tx_time_logical,
                version_type: v1.version_type,
                properties: v1.properties,
                provenance: None,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<TemporalIndexDataV1> for super::TemporalIndexData {
        fn from(v1: TemporalIndexDataV1) -> Self {
            super::TemporalIndexData {
                magic: v1.magic,
                // The converted entries are current-shape (provenance field
                // added by the `NodeVersionEntryV1`/`EdgeVersionEntryV1` `From`
                // impls above), so the upgraded struct must be stamped with
                // the current format version, not the legacy one it was read
                // as. Otherwise `decode_temporal_blob` would misdetect this
                // struct as legacy on the next load and misdecode it.
                version: super::super::MANIFEST_VERSION,
                node_versions: v1.node_versions.into_iter().map(Into::into).collect(),
                node_anchors: v1.node_anchors,
                edge_versions: v1.edge_versions.into_iter().map(Into::into).collect(),
                edge_anchors: v1.edge_anchors,
            }
        }
    }
}

// ============================================================================
// Legacy (pre-principal, pre-fidelity) Temporal Index Format (Issue #3350)
// ============================================================================

/// Frozen copies of the temporal index structs as they existed between
/// write-time provenance (Issue #3224, `MANIFEST_VERSION == 2`) and the
/// authenticated-principal provenance field (Issue #3350,
/// `MANIFEST_VERSION == 3`).
///
/// Same rationale as [`legacy_v1`]: `bitcode` is positional and
/// non-self-describing, so adding `principal` to [`PersistedProvenance`]
/// changed the wire layout of every entry that embeds it. These frozen
/// shapes let `super::temporal::load_temporal_index` fall back to the v2
/// layout and upgrade in memory (`principal: None`; the Issue #3387
/// `tx_end`/`tx_end_logical`/`prev_version`/`next_version` fidelity fields
/// also default to `None`, and the restore path falls back to heuristically
/// rebuilding chains and closures via `rebuild_version_chains`, exactly as
/// v2 binaries did).
///
/// Do not modify these types after they're introduced -- they exist purely
/// to describe historical on-disk bytes.
pub mod legacy_v2 {
    use super::{EdgeAnchorEntry, NodeAnchorEntry, PersistedPropertyMap, PersistedVersionType};
    use bitcode::{Decode, Encode};

    /// Pre-principal `PersistedProvenance` (`version == 2`): the Issue #3224
    /// shape without the `principal` field.
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct PersistedProvenanceV2 {
        /// Source system/identifier that produced the write, if any.
        pub source: Option<String>,
        /// Confidence in `[0.0, 1.0]`, if any.
        pub confidence: Option<f64>,
        /// Free-text note, if any.
        pub note: Option<String>,
        /// Correlation ID grouping co-committed writes, if any.
        pub correlation_id: Option<String>,
    }

    /// Pre-principal `TemporalIndexData` (`version == 2`).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct TemporalIndexDataV2 {
        /// Magic bytes: "GTMP"
        pub magic: [u8; 4],
        /// Format version (always 2 for this shape)
        pub version: u16,
        /// Node version entries
        pub node_versions: Vec<NodeVersionEntryV2>,
        /// Node anchor entries (unchanged since v1)
        pub node_anchors: Vec<NodeAnchorEntry>,
        /// Edge version entries
        pub edge_versions: Vec<EdgeVersionEntryV2>,
        /// Edge anchor entries (unchanged since v1)
        pub edge_anchors: Vec<EdgeAnchorEntry>,
    }

    /// Pre-principal `NodeVersionEntry` (provenance without `principal`,
    /// no tx-end / chain-link fields).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct NodeVersionEntryV2 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Node ID
        pub node_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start (unix timestamp)
        pub valid_from: i64,
        /// Valid time end (None = still valid)
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time (unix timestamp)
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type (delta or anchor)
        pub version_type: PersistedVersionType,
        /// Properties at this version
        pub properties: PersistedPropertyMap,
        /// Vector snapshot ID for provenance tracking
        pub vector_snapshot_id: Option<u64>,
        /// Write-time provenance bundle (pre-principal shape), if supplied.
        pub provenance: Option<PersistedProvenanceV2>,
    }

    /// Pre-principal `EdgeVersionEntry` (provenance without `principal`,
    /// no tx-end / chain-link fields).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct EdgeVersionEntryV2 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Edge ID
        pub edge_id: u64,
        /// Source node ID
        pub source_id: u64,
        /// Target node ID
        pub target_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start
        pub valid_from: i64,
        /// Valid time end
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type
        pub version_type: PersistedVersionType,
        /// Properties
        pub properties: PersistedPropertyMap,
        /// Write-time provenance bundle (pre-principal shape), if supplied.
        pub provenance: Option<PersistedProvenanceV2>,
    }

    impl From<PersistedProvenanceV2> for super::PersistedProvenance {
        fn from(v2: PersistedProvenanceV2) -> Self {
            super::PersistedProvenance {
                source: v2.source,
                confidence: v2.confidence,
                note: v2.note,
                correlation_id: v2.correlation_id,
                principal: None,
            }
        }
    }

    impl From<NodeVersionEntryV2> for super::NodeVersionEntry {
        fn from(v2: NodeVersionEntryV2) -> Self {
            super::NodeVersionEntry {
                version_id: v2.version_id,
                node_id: v2.node_id,
                label_idx: v2.label_idx,
                valid_from: v2.valid_from,
                valid_to: v2.valid_to,
                valid_from_logical: v2.valid_from_logical,
                valid_to_logical: v2.valid_to_logical,
                tx_time: v2.tx_time,
                tx_time_logical: v2.tx_time_logical,
                version_type: v2.version_type,
                properties: v2.properties,
                vector_snapshot_id: v2.vector_snapshot_id,
                provenance: v2.provenance.map(Into::into),
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<EdgeVersionEntryV2> for super::EdgeVersionEntry {
        fn from(v2: EdgeVersionEntryV2) -> Self {
            super::EdgeVersionEntry {
                version_id: v2.version_id,
                edge_id: v2.edge_id,
                source_id: v2.source_id,
                target_id: v2.target_id,
                label_idx: v2.label_idx,
                valid_from: v2.valid_from,
                valid_to: v2.valid_to,
                valid_from_logical: v2.valid_from_logical,
                valid_to_logical: v2.valid_to_logical,
                tx_time: v2.tx_time,
                tx_time_logical: v2.tx_time_logical,
                version_type: v2.version_type,
                properties: v2.properties,
                provenance: v2.provenance.map(Into::into),
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<TemporalIndexDataV2> for super::TemporalIndexData {
        fn from(v2: TemporalIndexDataV2) -> Self {
            super::TemporalIndexData {
                magic: v2.magic,
                // Stamp the upgraded struct with the current format version
                // (same rationale as the legacy_v1 upgrade above).
                version: super::super::MANIFEST_VERSION,
                node_versions: v2.node_versions.into_iter().map(Into::into).collect(),
                node_anchors: v2.node_anchors,
                edge_versions: v2.edge_versions.into_iter().map(Into::into).collect(),
                edge_anchors: v2.edge_anchors,
            }
        }
    }
}

// ============================================================================
// Legacy (pre-bi-temporal-fidelity) Temporal Index Format (Issue #3387)
// ============================================================================

/// Frozen copies of the temporal index structs as they existed between the
/// authenticated-principal provenance field (Issue #3350,
/// `MANIFEST_VERSION == 3`) and the bi-temporal fidelity fields
/// (Issue #3387, `MANIFEST_VERSION == 4`).
///
/// Same rationale as [`legacy_v1`]/[`legacy_v2`]: `bitcode` is positional
/// and non-self-describing, so appending the `tx_end`/`tx_end_logical`/
/// `prev_version`/`next_version` fields changes the wire layout and
/// `version == 3` files written by #3350-era binaries can no longer decode
/// as the current structs. These frozen shapes let
/// [`super::temporal::load_temporal_index`] fall back to the v3 layout and
/// upgrade in memory (fidelity fields `None`; the restore path then falls
/// back to heuristically rebuilding chains and closures via
/// `rebuild_version_chains`, exactly as v3 binaries did). The
/// principal-carrying provenance is preserved as-is: v3 and v4 share the
/// live [`PersistedProvenance`] shape.
///
/// Do not modify these types -- they exist purely to describe historical
/// on-disk bytes. Frozen reference: the live `NodeVersionEntry`/
/// `EdgeVersionEntry`/`TemporalIndexData` structs as of trunk commit
/// f2e02c6 (the #3421 merge, the last pre-#3387 trunk commit). If these
/// drift from those bytes, the legacy-read round-trip tests become
/// tautological.
pub mod legacy_v3 {
    use super::{
        EdgeAnchorEntry, NodeAnchorEntry, PersistedPropertyMap, PersistedProvenance,
        PersistedVersionType,
    };
    use bitcode::{Decode, Encode};

    /// Pre-fidelity `TemporalIndexData` (`version == 3`).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct TemporalIndexDataV3 {
        /// Magic bytes: "GTMP"
        pub magic: [u8; 4],
        /// Format version (always 3 for this shape)
        pub version: u16,
        /// Node version entries
        pub node_versions: Vec<NodeVersionEntryV3>,
        /// Node anchor entries (unchanged since v1)
        pub node_anchors: Vec<NodeAnchorEntry>,
        /// Edge version entries
        pub edge_versions: Vec<EdgeVersionEntryV3>,
        /// Edge anchor entries (unchanged since v1)
        pub edge_anchors: Vec<EdgeAnchorEntry>,
    }

    /// Pre-fidelity `NodeVersionEntry` (principal-carrying provenance, no
    /// tx-end / chain-link fields).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct NodeVersionEntryV3 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Node ID
        pub node_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start (unix timestamp)
        pub valid_from: i64,
        /// Valid time end (None = still valid)
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time (unix timestamp)
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type (delta or anchor)
        pub version_type: PersistedVersionType,
        /// Properties at this version
        pub properties: PersistedPropertyMap,
        /// Vector snapshot ID for provenance tracking
        pub vector_snapshot_id: Option<u64>,
        /// Write-time attributive provenance bundle (with `principal`).
        pub provenance: Option<PersistedProvenance>,
    }

    /// Pre-fidelity `EdgeVersionEntry` (principal-carrying provenance, no
    /// tx-end / chain-link fields).
    #[derive(Debug, Clone, Encode, Decode)]
    pub struct EdgeVersionEntryV3 {
        /// Unique version identifier (preserved from original)
        pub version_id: u64,
        /// Edge ID
        pub edge_id: u64,
        /// Source node ID
        pub source_id: u64,
        /// Target node ID
        pub target_id: u64,
        /// Label index in string interner
        pub label_idx: u32,
        /// Valid time start
        pub valid_from: i64,
        /// Valid time end
        pub valid_to: Option<i64>,
        /// Valid time start (logical counter)
        pub valid_from_logical: u32,
        /// Valid time end (logical counter)
        pub valid_to_logical: Option<u32>,
        /// Transaction time
        pub tx_time: i64,
        /// Transaction time (logical counter)
        pub tx_time_logical: u32,
        /// Version type
        pub version_type: PersistedVersionType,
        /// Properties
        pub properties: PersistedPropertyMap,
        /// Write-time attributive provenance bundle (with `principal`).
        pub provenance: Option<PersistedProvenance>,
    }

    impl From<NodeVersionEntryV3> for super::NodeVersionEntry {
        fn from(v3: NodeVersionEntryV3) -> Self {
            super::NodeVersionEntry {
                version_id: v3.version_id,
                node_id: v3.node_id,
                label_idx: v3.label_idx,
                valid_from: v3.valid_from,
                valid_to: v3.valid_to,
                valid_from_logical: v3.valid_from_logical,
                valid_to_logical: v3.valid_to_logical,
                tx_time: v3.tx_time,
                tx_time_logical: v3.tx_time_logical,
                version_type: v3.version_type,
                properties: v3.properties,
                vector_snapshot_id: v3.vector_snapshot_id,
                provenance: v3.provenance,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<EdgeVersionEntryV3> for super::EdgeVersionEntry {
        fn from(v3: EdgeVersionEntryV3) -> Self {
            super::EdgeVersionEntry {
                version_id: v3.version_id,
                edge_id: v3.edge_id,
                source_id: v3.source_id,
                target_id: v3.target_id,
                label_idx: v3.label_idx,
                valid_from: v3.valid_from,
                valid_to: v3.valid_to,
                valid_from_logical: v3.valid_from_logical,
                valid_to_logical: v3.valid_to_logical,
                tx_time: v3.tx_time,
                tx_time_logical: v3.tx_time_logical,
                version_type: v3.version_type,
                properties: v3.properties,
                provenance: v3.provenance,
                tx_end: None,
                tx_end_logical: None,
                prev_version: None,
                next_version: None,
            }
        }
    }

    impl From<TemporalIndexDataV3> for super::TemporalIndexData {
        fn from(v3: TemporalIndexDataV3) -> Self {
            super::TemporalIndexData {
                magic: v3.magic,
                // Stamp the upgraded struct with the current format version
                // (same rationale as the legacy_v1 upgrade above).
                version: super::super::MANIFEST_VERSION,
                node_versions: v3.node_versions.into_iter().map(Into::into).collect(),
                node_anchors: v3.node_anchors,
                edge_versions: v3.edge_versions.into_iter().map(Into::into).collect(),
                edge_anchors: v3.edge_anchors,
            }
        }
    }
}
