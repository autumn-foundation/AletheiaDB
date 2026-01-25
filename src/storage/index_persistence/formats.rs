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
